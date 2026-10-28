/* SPDX-License-Identifier: Apache-2.0 */
#define _GNU_SOURCE
#include "app_log.h"
#include "resize.h"
#include "user.h"
#include <errno.h>
#include <fcntl.h>
#include <linux/vm_sockets.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc > 1 && (!strcmp(argv[1], "--sandbox-probe") || !strcmp(argv[1], "--sandbox-lock") || !strcmp(argv[1], "--reuid"))) {
        return firecrab_sandbox_user(argc, argv);
    }
    if (argc == 2 && !strcmp(argv[1], "--version")) {
        puts("firecrab-guest-console 1");
        return 0;
    }
    if (argc == 2 && !strcmp(argv[1], "--app-log-check")) return 0;
    if (argc >= 3 && !strcmp(argv[1], "--app-run")) return firecrab_app_log_run(argc - 2, argv + 2);
    if (argc != 1) return 2;
    signal(SIGPIPE, SIG_IGN);
    int server;
    struct sockaddr_vm address = {0};
    address.svm_family = AF_VSOCK;
    address.svm_cid = VMADDR_CID_ANY;
    address.svm_port = 47100;
    /* Native init can start metrics before a modular vsock driver is ready. */
    for (;;) {
        server = socket(AF_VSOCK, SOCK_STREAM | SOCK_CLOEXEC, 0);
        if (server >= 0 && !bind(server, (struct sockaddr *)&address, sizeof address) && !listen(server, 4)) break;
        int error = errno;
        if (server >= 0) close(server);
        if (error == EAFNOSUPPORT || error == EPROTONOSUPPORT || error == ENODEV || error == EADDRNOTAVAIL || error == EADDRINUSE) {
            sleep(3);
            continue;
        }
        errno = error;
        perror("console vsock listen");
        return 1;
    }
    for (;;) {
        struct sockaddr_vm peer = {0};
        socklen_t length = sizeof peer;
        int client = accept4(server, (struct sockaddr *)&peer, &length, SOCK_CLOEXEC);
        if (client < 0) {
            if (errno == EINTR) continue;
            perror("console vsock accept");
            close(server);
            return 1;
        }
        /* A guest process cannot use this root-only operation through loopback. */
        if (length != sizeof peer || peer.svm_cid != VMADDR_CID_HOST) {
            close(client);
            continue;
        }
        struct timeval timeout = {.tv_sec = 1};
        if (setsockopt(client, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof timeout) ||
            setsockopt(client, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof timeout)) {
            close(client);
            continue;
        }
        int tty = open("/dev/ttyS0", O_RDWR | O_NOCTTY | O_CLOEXEC);
        /* An unavailable tty returns FCE1/EBADF and leaves the console working. */
        firecrab_console_resize(client, tty);
        if (tty >= 0) close(tty);
        close(client);
    }
}
