/* SPDX-License-Identifier: Apache-2.0 */
#define _POSIX_C_SOURCE 200809L
#include "app_log.h"
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#if defined(FIRECRAB_APP_LOG_TEST)
#include <sys/un.h>
#else
#include <linux/vm_sockets.h>
#endif

#ifndef MSG_NOSIGNAL
#define MSG_NOSIGNAL 0
#endif

#define KEY_LEN 32
#define CHUNK 4096
#define BACKLOG 64
#define RETRY_MS 1000
#define DRAIN_MS 2000

/* The app is its own process group so TERM from the VM env update reaches it. */
static volatile sig_atomic_t app_pid;
struct queued_chunk {
    unsigned char tag;
    size_t length;
    unsigned char data[CHUNK];
};
/* A bounded queue also retains early output while the vsock is starting. */
static struct queued_chunk pending[BACKLOG];
static void forward_signal(int signo) {
    int saved = errno;
    if (app_pid > 0) kill(-(pid_t)app_pid, signo);
    errno = saved;
}

static int64_t monotonic_ms(void) {
    struct timespec value;
    if (clock_gettime(CLOCK_MONOTONIC, &value)) return 0;
    return (int64_t)value.tv_sec * 1000 + value.tv_nsec / 1000000;
}

static int nonblocking(int fd) {
    int flags = fcntl(fd, F_GETFL, 0);
    return flags < 0 || fcntl(fd, F_SETFL, flags | O_NONBLOCK) < 0 ? -1 : 0;
}

static void close_link(int *fd, int *state) {
    if (*fd >= 0) close(*fd);
    *fd = -1;
    *state = 0;
}

/* Retry without reconnecting when no bytes could be sent. A partial frame
 * cannot be replayed on the same stream, so reconnect in that case. */
static int send_frame(int fd, unsigned char tag, const void *data, size_t length) {
    unsigned char header[5] = {tag, 0, 0, 0, 0};
    uint32_t size = htonl((uint32_t)length);
    memcpy(header + 1, &size, sizeof size);
    struct iovec items[2] = {{header, sizeof header}, {(void *)data, length}};
    struct msghdr message = {0};
    message.msg_iov = items;
    message.msg_iovlen = 2;
    ssize_t written = sendmsg(fd, &message, MSG_DONTWAIT | MSG_NOSIGNAL);
    if (written < 0 && (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR)) return 1;
    return written == (ssize_t)(sizeof header + length) ? 0 : -1;
}

static int start_link(void) {
#if defined(FIRECRAB_APP_LOG_TEST)
    const char *path = getenv("FIRECRAB_APP_LOG_TEST_SOCKET");
    if (!path || strlen(path) >= sizeof(((struct sockaddr_un *)0)->sun_path)) return -1;
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    struct sockaddr_un address = {0};
    address.sun_family = AF_UNIX;
    strcpy(address.sun_path, path);
#else
    int fd = socket(AF_VSOCK, SOCK_STREAM, 0);
    struct sockaddr_vm address = {0};
    address.svm_family = AF_VSOCK;
    address.svm_cid = VMADDR_CID_HOST;
    address.svm_port = 47101;
#endif
    if (fd < 0) return -1;
    (void)fcntl(fd, F_SETFD, FD_CLOEXEC);
    if (nonblocking(fd)) { close(fd); return -1; }
    if (connect(fd, (struct sockaddr *)&address, sizeof address) && errno != EINPROGRESS) {
        close(fd);
        return -1;
    }
    return fd;
}

static int read_key(unsigned char key[KEY_LEN]) {
#if defined(FIRECRAB_APP_LOG_TEST)
    const char *path = getenv("FIRECRAB_APP_LOG_TEST_KEY");
    if (!path) return -1;
#else
    const char *path = "/etc/firecrab/app-log.key";
#endif
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    size_t offset = 0;
    while (offset < KEY_LEN) {
        ssize_t n = read(fd, key + offset, KEY_LEN - offset);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) { close(fd); return -1; }
        offset += (size_t)n;
    }
    unsigned char extra;
    ssize_t n = read(fd, &extra, 1);
    close(fd);
    return n == 0 ? 0 : -1;
}

static void report_lost(int *fd, int *state, uint64_t *lost) {
    if (*state != 2 || !*lost) return;
    unsigned char count[8];
    for (int i = 7; i >= 0; --i) count[7 - i] = (unsigned char)(*lost >> (i * 8));
    int sent = send_frame(*fd, 3, count, sizeof count);
    if (sent < 0) close_link(fd, state);
    else if (sent == 0) *lost = 0;
}

/* When setup cannot run, retain the original app path and console behavior. */
static int fallback(char **argv) {
    execv(argv[0], argv);
    return 127;
}

int firecrab_app_log_run(int argc, char **argv) {
    if (argc < 1 || !argv[0]) return 2;
    unsigned char key[KEY_LEN];
    if (read_key(key)) return fallback(argv);
    int out[2], err[2];
    if (pipe(out)) return fallback(argv);
    if (pipe(err)) { close(out[0]); close(out[1]); return fallback(argv); }
    sigset_t blocked, previous;
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGTERM);
    sigaddset(&blocked, SIGINT);
    sigaddset(&blocked, SIGHUP);
    if (sigprocmask(SIG_BLOCK, &blocked, &previous)) {
        close(out[0]); close(out[1]); close(err[0]); close(err[1]);
        return fallback(argv);
    }
    pid_t child = fork();
    if (child < 0) {
        close(out[0]); close(out[1]); close(err[0]); close(err[1]);
        (void)sigprocmask(SIG_SETMASK, &previous, NULL);
        return fallback(argv);
    }
    if (!child) {
        (void)setpgid(0, 0);
        close(out[0]); close(err[0]);
        if (dup2(out[1], STDOUT_FILENO) < 0 || dup2(err[1], STDERR_FILENO) < 0) _exit(127);
        close(out[1]); close(err[1]);
        signal(SIGPIPE, SIG_DFL);
        (void)sigprocmask(SIG_SETMASK, &previous, NULL);
        execv(argv[0], argv);
        _exit(127);
    }
    app_pid = child;
    (void)setpgid(child, child);
    close(out[1]); close(err[1]);
    (void)nonblocking(out[0]); (void)nonblocking(err[0]);
    struct sigaction action = {0};
    action.sa_handler = forward_signal;
    sigemptyset(&action.sa_mask);
    (void)sigaction(SIGTERM, &action, NULL);
    (void)sigaction(SIGINT, &action, NULL);
    (void)sigaction(SIGHUP, &action, NULL);
    signal(SIGPIPE, SIG_IGN);
    (void)sigprocmask(SIG_SETMASK, &previous, NULL);

    int link = -1, state = 0, done = 0, result = 0;
    int64_t retry_at = 0, drain_until = 0;
    uint64_t lost = 0;
    size_t head = 0, count = 0;
    while (out[0] >= 0 || err[0] >= 0 || !done || count) {
        int64_t now = monotonic_ms();
        if (!done) {
            pid_t waited = waitpid(child, &result, WNOHANG);
            if (waited == child || (waited < 0 && errno == ECHILD)) {
                done = 1; drain_until = now + DRAIN_MS;
            }
        }
        if (done && now >= drain_until) break;
        if (link < 0 && now >= retry_at) {
            link = start_link();
            if (link >= 0) state = 1;
            else retry_at = now + RETRY_MS;
        }
        struct pollfd fds[3] = {{out[0], POLLIN, 0}, {err[0], POLLIN, 0}, {link, (state == 1 || (state == 2 && (count || lost))) ? POLLOUT : 0, 0}};
        if (poll(fds, 3, 100) < 0 && errno != EINTR) break;
        if (link >= 0 && state == 1 && fds[2].revents) {
            int error = 0; socklen_t size = sizeof error;
            if (getsockopt(link, SOL_SOCKET, SO_ERROR, &error, &size) || error) {
                close_link(&link, &state); retry_at = now + RETRY_MS;
            } else {
                int sent = send_frame(link, 0, key, sizeof key);
                if (sent < 0) { close_link(&link, &state); retry_at = now + RETRY_MS; }
                else if (sent == 0) { state = 2; report_lost(&link, &state, &lost); }
            }
        } else if (link >= 0 && state == 2 && (fds[2].revents & (POLLERR | POLLHUP | POLLNVAL))) {
            close_link(&link, &state); retry_at = now + RETRY_MS;
        }
        for (int i = 0; i < 2; ++i) {
            int *fd = i == 0 ? &out[0] : &err[0];
            if (*fd < 0 || !(fds[i].revents & (POLLIN | POLLHUP | POLLERR))) continue;
            unsigned char buffer[CHUNK];
            ssize_t n = read(*fd, buffer, sizeof buffer);
            if (n > 0) {
                if (count == BACKLOG) {
                    lost += pending[head].length;
                    head = (head + 1) % BACKLOG;
                    --count;
                }
                size_t tail = (head + count++) % BACKLOG;
                pending[tail].tag = (unsigned char)(i + 1);
                pending[tail].length = (size_t)n;
                memcpy(pending[tail].data, buffer, (size_t)n);
            } else if (n == 0 || (errno != EINTR && errno != EAGAIN)) {
                close(*fd); *fd = -1;
            }
        }
        report_lost(&link, &state, &lost);
        while (state == 2 && count) {
            struct queued_chunk *chunk = &pending[head];
            int sent = send_frame(link, chunk->tag, chunk->data, chunk->length);
            if (sent < 0) {
                close_link(&link, &state); retry_at = now + RETRY_MS;
                break;
            }
            if (sent > 0) break;
            head = (head + 1) % BACKLOG;
            --count;
        }
    }
    if (out[0] >= 0) close(out[0]);
    if (err[0] >= 0) close(err[0]);
    close_link(&link, &state);
    if (!done) while (waitpid(child, &result, 0) < 0 && errno == EINTR) {}
    app_pid = 0;
    if (WIFEXITED(result)) return WEXITSTATUS(result);
    if (WIFSIGNALED(result)) return 128 + WTERMSIG(result);
    return 1;
}
