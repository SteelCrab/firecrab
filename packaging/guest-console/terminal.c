/* SPDX-License-Identifier: Apache-2.0 */
#define _GNU_SOURCE
#include "workspace.h"
#include "workspace_process.h"
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <poll.h>
#include <pwd.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/types.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#ifdef __linux__
#include <sys/mount.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/syscall.h>

#define HEADER 40U
#define META 88U
#define CHUNK 32768U
#ifdef FIRECRAB_WORKSPACE_TEST
#define RETAIN (64U * 1024U)
#else
#define RETAIN (8U * 1024U * 1024U)
#endif
/* Root-owned records survive a boot. A session ID never executes twice. */
struct request {
  unsigned char header[HEADER], data[16385];
  uint32_t action, size;
  uint64_t cursor;
};
static int response(int fd, const void *data, uint32_t size, unsigned error) {
  unsigned char header[8];
  memcpy(header, error ? "FCTE" : "FCTR", 4);
  fcp_p32(header + 4, error ? error : size);
  return fcp_io_all(fd, header, 8, 1) ||
         (!error && size && fcp_io_all(fd, (void *)data, size, 1));
}
static int rejected(int fd) {
  unsigned code = 7;
  switch (errno) {
  case EINVAL:
  case ENAMETOOLONG:
    code = 1;
    break;
  case ENOENT:
    code = 2;
    break;
  case EEXIST:
  case ESTALE:
    code = 3;
    break;
  case EAGAIN:
  case EBUSY:
    code = 4;
    break;
  case EACCES:
  case EPERM:
  case ELOOP:
  case ENOTDIR:
    code = 5;
    break;
  case ENOSPC:
  case EFBIG:
    code = 6;
    break;
  case ENODEV:
  case ENOTSUP:
    code = 8;
    break;
  }
  return response(fd, NULL, 0, code);
}
static int read_request(int fd, struct request *r) {
  if (fcp_io_all(fd, r->header, HEADER, 0) || memcmp(r->header, "FCT1", 4)) {
    errno = EINVAL;
    return -1;
  }
  r->action = fcp_n32(r->header + 4);
  r->cursor = fcp_n64(r->header + 24);
  r->size = fcp_n32(r->header + 36);
  unsigned rows = (unsigned)r->header[32] * 256 + r->header[33],
           cols = (unsigned)r->header[34] * 256 + r->header[35];
  if (r->action < 1 || r->action > 5 || r->size > 16384 ||
      !memcmp(r->header + 8, (unsigned char[16]){0}, 16) ||
      ((r->action == 1 || r->action == 4) &&
       (rows < 2 || rows > 500 || cols < 2 || cols > 1000)) ||
      (r->action == 1 && r->size > 1024) ||
      (r->action != 1 && r->action != 3 && r->size)) {
    errno = EINVAL;
    return -1;
  }
  if (fcp_io_all(fd, r->data, r->size, 0))
    return -1;
  r->data[r->size] = 0;
  if (r->action == 1 && strlen((char *)r->data) != r->size) {
    errno = EINVAL;
    return -1;
  }
  return 0;
}
static int socket_path(int dir, char path[108]) {
  return snprintf(path, 108, "/proc/self/fd/%d/control.sock", dir) >= 108 ? -1
                                                                          : 0;
}
static int read_meta(int dir, unsigned char meta[META]) {
  if (fcp_read_exact(dir, "state", meta, META) || memcmp(meta, "FCTS", 4) ||
      fcp_n32(meta + 4) > 3 || fcp_n32(meta + 8) < 100000) {
    errno = ESTALE;
    return -1;
  }
  return 0;
}
static int save_meta(int dir, unsigned char meta[META]) {
  return fcp_atomic_record(dir, "state", meta, META);
}
static int same_boot(const unsigned char meta[META]) {
  unsigned char boot[16];
  return !fcp_boot_id(boot) && !memcmp(boot, meta + 40, 16);
}
static int live_server(const unsigned char meta[META]) {
  return same_boot(meta) && fcp_n32(meta + 4) == 1 &&
         fcp_ticks((pid_t)fcp_n32(meta + 16)) == fcp_n64(meta + 24);
}
static int chunk_start(const char *name, uint64_t *value) {
  if (strlen(name) != 26 || strcmp(name + 20, ".chunk"))
    return -1;
  for (unsigned i = 0; i < 20; i++)
    if (name[i] < '0' || name[i] > '9')
      return -1;
  char *end;
  errno = 0;
  unsigned long long n = strtoull(name, &end, 10);
  if (errno || end != name + 20)
    return -1;
  *value = n;
  return 0;
}
static int record_output(int dir, int chunks, unsigned char meta[META],
                         void *data, unsigned n) {
  uint64_t end = fcp_n64(meta + 64);
  if (UINT64_MAX - end < n) {
    errno = EFBIG;
    return -1;
  }
  char name[40];
  snprintf(name, sizeof name, "%020llu.chunk", (unsigned long long)end);
  int fd = fcp_file_at(chunks, name, O_WRONLY | O_CREAT | O_EXCL);
  if (fd < 0)
    return -1;
  int failed = fcp_io_all(fd, data, n, 1) || fsync(fd) || fsync(chunks);
  int error = errno;
  close(fd);
  errno = error;
  if (failed)
    return -1;
  end += n;
  uint64_t threshold = end > RETAIN ? end - RETAIN : 0, begin = end;
  DIR *list =
      fdopendir(openat(chunks, ".", O_RDONLY | O_DIRECTORY | O_CLOEXEC));
  if (!list)
    return -1;
  struct dirent *e;
  errno = 0;
  while ((e = readdir(list))) {
    uint64_t start;
    if (!chunk_start(e->d_name, &start) && start >= threshold && start < begin)
      begin = start;
  }
  closedir(list);
  fcp_p64(meta + 56, begin);
  fcp_p64(meta + 64, end);
  if (begin)
    fcp_p32(meta + 76, fcp_n32(meta + 76) | 1);
  if (save_meta(dir, meta))
    return -1;
  list = fdopendir(openat(chunks, ".", O_RDONLY | O_DIRECTORY | O_CLOEXEC));
  if (!list)
    return -1;
  while ((e = readdir(list))) {
    uint64_t start;
    if (!chunk_start(e->d_name, &start) && start < begin &&
        unlinkat(chunks, e->d_name, 0)) {
      error = errno;
      closedir(list);
      errno = error;
      return -1;
    }
  }
  closedir(list);
  return fsync(chunks);
}
static int send_output(int client, int dir, int chunks,
                       const unsigned char meta[META], uint64_t cursor) {
  (void)dir;
  uint64_t begin = fcp_n64(meta + 56), end = fcp_n64(meta + 64);
  if (cursor > end) {
    errno = EINVAL;
    return rejected(client);
  }
  if (cursor < begin)
    cursor = begin;
  unsigned char result[META + 8 + CHUNK];
  memcpy(result, meta, META);
  fcp_p64(result + META, cursor);
  unsigned size = 0;
  /* Chunks are immutable. Read only the committed cursor range. */
  if (cursor < end) {
    DIR *d = fdopendir(openat(chunks, ".", O_RDONLY | O_DIRECTORY | O_CLOEXEC));
    if (!d)
      return rejected(client);
    struct dirent *e;
    uint64_t selected = UINT64_MAX;
    char name[40] = {0};
    while ((e = readdir(d))) {
      uint64_t start;
      if (!chunk_start(e->d_name, &start) && start <= cursor &&
          (selected == UINT64_MAX || start > selected)) {
        selected = start;
        strcpy(name, e->d_name);
      }
    }
    closedir(d);
    if (selected == UINT64_MAX) {
      errno = ESTALE;
      return rejected(client);
    }
    int fd = fcp_file_at(chunks, name, O_RDONLY);
    if (fd < 0)
      return rejected(client);
    struct stat s;
    if (fstat(fd, &s) || s.st_size <= 0 || s.st_size > CHUNK ||
        cursor - selected >= (uint64_t)s.st_size) {
      close(fd);
      errno = ESTALE;
      return rejected(client);
    }
    uint64_t available = (uint64_t)s.st_size - (cursor - selected);
    if (available > end - cursor)
      available = end - cursor;
    ssize_t n = pread(fd, result + META + 8, (size_t)available,
                      (off_t)(cursor - selected));
    int error = errno;
    close(fd);
    errno = error;
    if (n != (ssize_t)available)
      return rejected(client);
    size = (unsigned)n;
  }
  return response(client, result, META + 8 + size, 0);
}
static void shell(int master, int cwd, int home, uid_t uid, gid_t gid,
                  const char *home_path) {
  char *name = ptsname(master);
  if (!name)
    _exit(77);
  if (setsid() < 0)
    _exit(77);
  int slave = open(name, O_RDWR | O_NOCTTY);
  if (slave < 0 || ioctl(slave, TIOCSCTTY, 0))
    _exit(77);
  if (fchown(slave, uid, gid) || fchmod(slave, 0600))
    _exit(77);
  for (int i = 0; i < 3; i++)
    if (dup2(slave, i) < 0)
      _exit(77);
  if (fchdir(cwd) || fchown(home, uid, gid) || fchmod(home, 0700))
    _exit(77);
  struct rlimit nofile = {256, 256}, processes = {256, 256}, core = {0, 0};
  setrlimit(RLIMIT_NOFILE, &nofile);
  setrlimit(RLIMIT_NPROC, &processes);
  setrlimit(RLIMIT_CORE, &core);
  umask(0007);
  if (prctl(PR_SET_KEEPCAPS, 0, 0, 0, 0) ||
      prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) || setgroups(0, NULL) ||
      setresgid(gid, gid, gid) || setresuid(uid, uid, uid))
    _exit(77);
  fcp_close_except(-1, -1, -1, -1, -1, -1);
  clearenv();
  setenv("PATH", "/usr/local/bin:/usr/bin:/bin", 1);
  setenv("HOME", home_path, 1);
  setenv("TERM", "xterm-256color", 1);
  setenv("LANG", "C.UTF-8", 1);
  setenv("USER", "firecrab", 1);
  setenv("LOGNAME", "firecrab", 1);
  signal(SIGHUP, SIG_DFL);
  signal(SIGPIPE, SIG_DFL);
  execl("/bin/sh", "sh", "-i", (char *)NULL);
  _exit(127);
}
static int handle(int client, int dir, int chunks, int master,
                  unsigned char meta[META], int *stop) {
  struct ucred peer;
  socklen_t length = sizeof peer;
  struct request r;
  if (getsockopt(client, SOL_SOCKET, SO_PEERCRED, &peer, &length) ||
      length != sizeof peer || peer.uid != 0) {
    errno = EACCES;
    return rejected(client);
  }
  if (read_request(client, &r))
    return rejected(client);
  unsigned char expected[16];
  if (fcp_read_exact(dir, "session", expected, 16) ||
      memcmp(expected, r.header + 8, 16)) {
    errno = ESTALE;
    return rejected(client);
  }
  if (r.action == 1) {
    errno = EEXIST;
    return rejected(client);
  }
  if (r.action == 3) {
    ssize_t n = write(master, r.data, r.size);
    if (n != (ssize_t)r.size)
      return response(client, NULL, 0, 9);
  } else if (r.action == 4) {
    struct winsize size = {
        .ws_row = (unsigned short)((unsigned)r.header[32] * 256 + r.header[33]),
        .ws_col =
            (unsigned short)((unsigned)r.header[34] * 256 + r.header[35])};
    if (ioctl(master, TIOCSWINSZ, &size))
      return rejected(client);
    fcp_p32(meta + 80, size.ws_row);
    fcp_p32(meta + 84, size.ws_col);
    if (save_meta(dir, meta)) return response(client, NULL, 0, 9);
  } else if (r.action == 5) {
    *stop = 1;
  }
  return send_output(client, dir, chunks, meta, r.cursor);
}
static void supervise(int root, int marker, int dir, int cwd, int lock,
                      struct request *r, unsigned char meta[META],
                      const char *home_path) {
  close(root);
  close(marker);
  close(lock);
  if (setsid() < 0 || prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0))
    _exit(77);
  signal(SIGHUP, SIG_IGN);
  signal(SIGPIPE, SIG_IGN);
  int null = open("/dev/null", O_RDWR);
  if (null < 0)
    _exit(77);
  for (int i = 0; i < 3; i++)
    if (dup2(null, i) < 0)
      _exit(77);
  if (null > 2)
    close(null);
  int home = fcp_dir_at(dir, "home", 0700, 1),
      chunks = fcp_dir_at(dir, "chunks", 0700, 1);
  int master = posix_openpt(O_RDWR | O_NOCTTY | O_CLOEXEC | O_NONBLOCK),
      server = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
  if (home < 0 || chunks < 0 || master < 0 || server < 0 || grantpt(master) ||
      unlockpt(master))
    _exit(77);
  struct sockaddr_un address = {.sun_family = AF_UNIX};
  socket_path(dir, address.sun_path);
  if (bind(server, (struct sockaddr *)&address, sizeof address) ||
      fchmodat(dir, "control.sock", 0600, 0) || listen(server, 16))
    _exit(77);
  struct winsize size = {
      .ws_row = (unsigned short)((unsigned)r->header[32] * 256 + r->header[33]),
      .ws_col =
          (unsigned short)((unsigned)r->header[34] * 256 + r->header[35])};
  if (ioctl(master, TIOCSWINSZ, &size))
    _exit(77);
  fcp_p32(meta + 80, size.ws_row);
  fcp_p32(meta + 84, size.ws_col);
  fcp_close_except(dir, cwd, home, master, server, chunks);
  pid_t child = fork();
  if (child < 0)
    _exit(77);
  if (!child)
    shell(master, cwd, home, (uid_t)fcp_n32(meta + 8), (gid_t)fcp_n32(meta + 12),
          home_path);
  close(cwd);
  close(home);
  fcp_p32(meta + 4, 1);
  fcp_p32(meta + 16, (uint32_t)getpid());
  fcp_p32(meta + 20, (uint32_t)child);
  fcp_p64(meta + 24, fcp_ticks(getpid()));
  fcp_p64(meta + 32, fcp_ticks(child));
  if (!fcp_n64(meta + 24) || !fcp_n64(meta + 32) || save_meta(dir, meta)) {
    fcp_terminate((uid_t)fcp_n32(meta + 8));
    _exit(77);
  }
  int stop = 0, failed = 0, exit_code = 0, exited = 0;
  for (;;) {
    struct pollfd events[2] = {{.fd = master, .events = POLLIN},
                               {.fd = server, .events = POLLIN}};
    int ready = poll(events, 2, 100);
    if (ready < 0 && errno != EINTR) {
      failed = 1;
      break;
    }
    if (events[0].revents & POLLIN) {
      unsigned char bytes[CHUNK];
      ssize_t n = read(master, bytes, sizeof bytes);
      if (n > 0 && record_output(dir, chunks, meta, bytes, (unsigned)n)) {
        failed = 1;
        break;
      }
    }
    if (events[1].revents & POLLIN) {
      int client = accept4(server, NULL, NULL, SOCK_CLOEXEC);
      if (client >= 0) {
        struct timeval timeout = {.tv_sec = 2};
        setsockopt(client, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof timeout);
        setsockopt(client, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof timeout);
        handle(client, dir, chunks, master, meta, &stop);
        close(client);
      }
    }
    int status;
    pid_t ended = waitpid(child, &status, WNOHANG);
    if (ended == child) {
      exited = 1;
      exit_code =
          WIFEXITED(status) ? WEXITSTATUS(status) : 128 + WTERMSIG(status);
      break;
    }
    if (stop) {
      exit_code = 143;
      break;
    }
  }
  if (fcp_terminate((uid_t)fcp_n32(meta + 8)))
    failed = 1;
  /* Drain the bounded PTY kernel buffer before recording the final cursor. */
  for (unsigned i = 0; i < 64; i++) {
    unsigned char bytes[CHUNK];
    ssize_t n = read(master, bytes, sizeof bytes);
    if (n <= 0)
      break;
    if (record_output(dir, chunks, meta, bytes, (unsigned)n)) {
      failed = 1;
      break;
    }
  }
  fcp_p32(meta + 4, failed ? 3 : 2);
  fcp_p32(meta + 72, (exited || stop) ? (uint32_t)exit_code : UINT32_MAX);
  if (failed)
    fcp_p32(meta + 76, fcp_n32(meta + 76) | 2);
  save_meta(dir, meta);
  close(master);
  close(server);
  unlinkat(dir, "control.sock", 0);
  fsync(dir);
  _exit(failed ? 77 : 0);
}

static int offline_output(int dir, int chunks, const struct request *r) {
  unsigned char meta[META];
  if (read_meta(dir, meta))
    return rejected(STDOUT_FILENO);
  if (fcp_n32(meta + 4) == 0)
    return response(STDOUT_FILENO, NULL, 0, 9);
  if (fcp_n32(meta + 4) == 1 && live_server(meta)) {
    errno = EBUSY;
    return rejected(STDOUT_FILENO);
  }
  if (fcp_n32(meta + 4) == 1) {
    fcp_p32(meta + 4, 3);
    fcp_p32(meta + 76, same_boot(meta) ? fcp_n32(meta + 76) | 2 : fcp_n32(meta + 76) & ~2U);
    if (save_meta(dir, meta))
      return rejected(STDOUT_FILENO);
  }
  if (r->action == 5 && same_boot(meta)) {
    if (fcp_terminate((uid_t)fcp_n32(meta + 8)))
      return rejected(STDOUT_FILENO);
    fcp_p32(meta + 76, fcp_n32(meta + 76) & ~2U);
    if (save_meta(dir, meta))
      return rejected(STDOUT_FILENO);
  }
  return send_output(STDOUT_FILENO, dir, chunks, meta, r->cursor);
}

int firecrab_workspace_terminal(int argc, char **argv) {
  umask(0077);
  signal(SIGPIPE, SIG_IGN);
  struct request r;
  int root = -1, marker = -1, lock = -1, terminals = -1, dir = -1, chunks = -1,
      cwd = -1, client = -1, result = -1;
  if (geteuid() != 0) {
    errno = EACCES;
    return rejected(STDOUT_FILENO);
  }
  if (read_request(STDIN_FILENO, &r))
    return rejected(STDOUT_FILENO);
  unsigned char extra;
  if (read(STDIN_FILENO, &extra, 1) != 0) {
    errno = EINVAL;
    return rejected(STDOUT_FILENO);
  }
  if (firecrab_workspace_open(argc, argv, &root, &marker, &lock))
    goto done;
  if (r.action == 1) {
    int probe = (int)syscall(SYS_pidfd_open, getpid(), 0);
    if (probe < 0) { errno = ENOTSUP; goto done; }
    int supported = syscall(SYS_pidfd_send_signal, probe, 0, NULL, 0) == 0;
    close(probe);
    if (!supported) { errno = ENOTSUP; goto done; }
  }
  terminals = fcp_dir_at(marker, "terminals", 0711, r.action == 1);
  if (terminals < 0)
    goto done;
  char id[33];
  for (unsigned i = 0; i < 16; i++)
    snprintf(id + i * 2, 3, "%02x", r.header[8 + i]);
  dir = fcp_dir_at(terminals, id, 0711, 0);
  if (dir < 0 && errno == ENOENT && r.action == 1) {
    /* Count retained records before starting anything; no silent history
     * eviction. */
    DIR *list = fdopendir(dup(terminals));
    if (!list)
      goto done;
    unsigned count = 0;
    struct dirent *e;
    while ((e = readdir(list)))
      if (strcmp(e->d_name, ".") && strcmp(e->d_name, ".."))
        count++;
    closedir(list);
    if (count >= 32) {
      errno = ENOSPC;
      goto done;
    }
    cwd = firecrab_workspace_directory(root, (char *)r.data);
    if (cwd < 0)
      goto done;
    uid_t uid;
    gid_t gid;
    if (fcp_group_prepare(root, marker, argv[2], &gid) ||
        fcp_reserve_uid(marker, r.header + 8, &uid))
      goto done;
    dir = fcp_dir_at(terminals, id, 0711, 1);
    if (dir < 0)
      goto done;
    unsigned char meta[META] = {0};
    memcpy(meta, "FCTS", 4);
    fcp_p32(meta + 8, uid);
    fcp_p32(meta + 12, gid);
    fcp_p32(meta + 72, UINT32_MAX);
    if (fcp_boot_id(meta + 40) || fcp_atomic_record(dir, "request", r.header, HEADER) ||
        fcp_atomic_record(dir, "cwd", r.data, r.size) ||
        fcp_atomic_record(dir, "session", r.header + 8, 16) || save_meta(dir, meta))
      goto done;
    char target[64], base[2048], home_path[2112];
    snprintf(target, sizeof target, "/proc/self/fd/%d", dir);
    ssize_t length = readlink(target, base, sizeof base - 1);
    if (length < 0)
      goto done;
    base[length] = 0;
    if (snprintf(home_path, sizeof home_path, "%s/home", base) >=
        (int)sizeof home_path) {
      errno = ENAMETOOLONG;
      goto done;
    }
    pid_t pid = fork();
    if (pid < 0)
      goto done;
    if (!pid)
      supervise(root, marker, dir, cwd, lock, &r, meta, home_path);
    struct timespec pause = {0, 10000000};
    for (unsigned i = 0; i < 200; i++) {
      if (!read_meta(dir, meta) && fcp_n32(meta + 4) != 0)
        break;
      int status;
      if (waitpid(pid, &status, WNOHANG) == pid) {
        errno = EIO;
        goto done;
      }
      nanosleep(&pause, NULL);
    }
  }
  if (dir < 0)
    goto done;
  unsigned char meta[META];
  if (read_meta(dir, meta))
    goto done;
  if (r.action == 1) {
    unsigned char header[HEADER], cwd_bytes[1024];
    if (fcp_read_exact(dir, "request", header, HEADER) ||
        memcmp(header, r.header, HEADER) ||
        fcp_read_exact(dir, "cwd", cwd_bytes, r.size) ||
        memcmp(cwd_bytes, r.data, r.size)) {
      errno = ESTALE;
      goto done;
    }
    r.action = 2;
    r.size = 0;
    fcp_p32(r.header + 4, 2);
    fcp_p32(r.header + 36, 0);
  }
  chunks = fcp_dir_at(dir, "chunks", 0700, 0);
  if (!live_server(meta)) {
    if (r.action != 2 && r.action != 5) {
      errno = ESTALE;
      goto done;
    }
    result = offline_output(dir, chunks, &r);
    goto done;
  }
  struct sockaddr_un address = {.sun_family = AF_UNIX};
  socket_path(dir, address.sun_path);
  client = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
  if (client < 0)
    goto done;
  struct timeval timeout = {.tv_sec = 4};
  setsockopt(client, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof timeout);
  setsockopt(client, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof timeout);
  if (connect(client, (struct sockaddr *)&address, sizeof address))
    goto transport_failed;
  struct ucred peer;
  socklen_t length = sizeof peer;
  if (getsockopt(client, SOL_SOCKET, SO_PEERCRED, &peer, &length) ||
      peer.uid != 0 || peer.pid != (pid_t)fcp_n32(meta + 16) ||
      fcp_ticks(peer.pid) != fcp_n64(meta + 24)) {
    errno = EACCES;
    goto done;
  }
  if (fcp_io_all(client, r.header, HEADER, 1) || fcp_io_all(client, r.data, r.size, 1))
    goto transport_failed;
  unsigned char header[8], payload[META + 8 + CHUNK];
  if (fcp_io_all(client, header, 8, 0))
    goto transport_failed;
  uint32_t size = fcp_n32(header + 4);
  if (!memcmp(header, "FCTE", 4) && size >= 1 && size <= 9) {
    result = response(STDOUT_FILENO, NULL, 0, size);
    goto done;
  }
  if (memcmp(header, "FCTR", 4) || size < META + 8 || size > sizeof payload ||
      fcp_io_all(client, payload, size, 0)) {
    errno = ESTALE;
    goto done;
  }
  result = response(STDOUT_FILENO, payload, size, 0);
  goto done;
transport_failed:
  result = (r.action == 2 || r.action == 5)
               ? offline_output(dir, chunks, &r)
               : response(STDOUT_FILENO, NULL, 0, 9);
done: {
  int error = errno;
  int descriptors[] = {client, cwd, chunks, dir, terminals, lock, marker, root};
  for (unsigned i = 0; i < sizeof descriptors / sizeof descriptors[0]; i++)
    if (descriptors[i] >= 0)
      close(descriptors[i]);
  errno = error;
  return result < 0 ? rejected(STDOUT_FILENO) : result;
}
}
#else
int firecrab_workspace_terminal(int argc, char **argv) {
  (void)argc;
  (void)argv;
  return 77;
}
#endif
