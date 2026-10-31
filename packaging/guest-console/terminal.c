/* SPDX-License-Identifier: Apache-2.0 */
#define _GNU_SOURCE
#include "workspace.h"
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
static uint32_t n32(const unsigned char *p) {
  return (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 8 |
         p[3];
}
static uint64_t n64(const unsigned char *p) {
  return (uint64_t)n32(p) << 32 | n32(p + 4);
}
static void p32(unsigned char *p, uint32_t n) {
  for (int i = 3; i >= 0; i--) {
    p[i] = (unsigned char)n;
    n >>= 8;
  }
}
static void p64(unsigned char *p, uint64_t n) {
  p32(p, (uint32_t)(n >> 32));
  p32(p + 4, (uint32_t)n);
}
static int io_all(int fd, void *buf, size_t length, int writing) {
  unsigned char *p = buf;
  while (length) {
    ssize_t n = writing ? write(fd, p, length) : read(fd, p, length);
    if (n < 0 && errno == EINTR)
      continue;
    if (n <= 0) {
      if (!n)
        errno = EIO;
      return -1;
    }
    p += n;
    length -= (size_t)n;
  }
  return 0;
}
static int response(int fd, const void *data, uint32_t size, unsigned error) {
  unsigned char header[8];
  memcpy(header, error ? "FCTE" : "FCTR", 4);
  p32(header + 4, error ? error : size);
  return io_all(fd, header, 8, 1) ||
         (!error && size && io_all(fd, (void *)data, size, 1));
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
  if (io_all(fd, r->header, HEADER, 0) || memcmp(r->header, "FCT1", 4)) {
    errno = EINVAL;
    return -1;
  }
  r->action = n32(r->header + 4);
  r->cursor = n64(r->header + 24);
  r->size = n32(r->header + 36);
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
  if (io_all(fd, r->data, r->size, 0))
    return -1;
  r->data[r->size] = 0;
  if (r->action == 1 && strlen((char *)r->data) != r->size) {
    errno = EINVAL;
    return -1;
  }
  return 0;
}
static int dir_at(int parent, const char *name, mode_t mode, int create) {
  if (create && mkdirat(parent, name, mode) && errno != EEXIST)
    return -1;
  int fd =
      openat(parent, name, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
  struct stat s;
  if (fd >= 0 && (fstat(fd, &s) || s.st_uid != 0 || (s.st_mode & 0022))) {
    close(fd);
    errno = EACCES;
    return -1;
  }
  if (fd >= 0 && create && (fchmod(fd, mode) || fsync(parent))) {
    int e = errno;
    close(fd);
    errno = e;
    return -1;
  }
  return fd;
}
static int file_at(int dir, const char *name, int flags) {
  int fd = openat(dir, name, flags | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK, 0600);
  struct stat s;
  if (fd >= 0 && (fstat(fd, &s) || !S_ISREG(s.st_mode) || s.st_uid != 0 ||
                  s.st_nlink != 1 || (s.st_mode & 0077))) {
    close(fd);
    errno = EACCES;
    return -1;
  }
  return fd;
}
static int read_exact(int dir, const char *name, void *bytes, size_t n) {
  int fd = file_at(dir, name, O_RDONLY);
  if (fd < 0)
    return -1;
  struct stat s;
  int result =
      fstat(fd, &s) || s.st_size != (off_t)n || io_all(fd, bytes, n, 0);
  int error = errno;
  close(fd);
  errno = error;
  return result ? -1 : 0;
}
static int atomic_record(int dir, const char *name, const void *data,
                         size_t size) {
  char temp[64];
  if (snprintf(temp, sizeof temp, "%s.tmp", name) >= (int)sizeof temp) {
    errno = EINVAL;
    return -1;
  }
  int fd = file_at(dir, temp, O_WRONLY | O_CREAT | O_TRUNC);
  if (fd < 0)
    return -1;
  int result = io_all(fd, (void *)data, size, 1) || fsync(fd);
  int error = errno;
  close(fd);
  if (!result && (renameat(dir, temp, dir, name) || fsync(dir))) {
    result = -1;
    error = errno;
  }
  errno = error;
  return result;
}
static uint64_t ticks(pid_t pid) {
  char path[64], bytes[4096];
  snprintf(path, sizeof path, "/proc/%ld/stat", (long)pid);
  int fd = open(path, O_RDONLY | O_CLOEXEC);
  if (fd < 0)
    return 0;
  ssize_t n = read(fd, bytes, sizeof bytes - 1);
  close(fd);
  if (n <= 0)
    return 0;
  bytes[n] = 0;
  char *p = strrchr(bytes, ')');
  if (!p || p[1] != ' ' || p[2] == 'Z' || p[2] == 'X')
    return 0;
  p += 2;
  for (int field = 3; field < 22; field++) {
    p = strchr(p, ' ');
    if (!p)
      return 0;
    p++;
  }
  char *end;
  errno = 0;
  unsigned long long value = strtoull(p, &end, 10);
  return errno || end == p ? 0 : (uint64_t)value;
}
static int boot_id(unsigned char id[16]) {
  char text[40];
  int fd = open("/proc/sys/kernel/random/boot_id", O_RDONLY | O_CLOEXEC);
  if (fd < 0)
    return -1;
  ssize_t n = read(fd, text, sizeof text - 1);
  close(fd);
  if (n < 36)
    return -1;
  text[n] = 0;
  unsigned index = 0;
  for (unsigned i = 0; i < 36;) {
    if (text[i] == '-') {
      i++;
      continue;
    }
    unsigned b;
    if (index >= 16 || sscanf(text + i, "%2x", &b) != 1)
      return -1;
    id[index++] = (unsigned char)b;
    i += 2;
  }
  return index == 16 ? 0 : -1;
}
/* The retained ledger prevents UID reuse. pidfds prevent signalling a recycled
 * PID. */
static int process_uid(pid_t pid, uid_t uid) {
  char path[64], bytes[4096];
  snprintf(path, sizeof path, "/proc/%ld/status", (long)pid);
  int fd = open(path, O_RDONLY | O_CLOEXEC);
  if (fd < 0)
    return errno == ENOENT || errno == ESRCH ? 0 : -1;
  ssize_t n = read(fd, bytes, sizeof bytes - 1);
  int error = errno;
  close(fd);
  errno = error;
  if (n <= 0)
    return n == 0 || errno == ESRCH || errno == ENOENT ? 0 : -1;
  bytes[n] = 0;
  char *line = strstr(bytes, "\nUid:");
  unsigned r, e, s, f;
  if (!line || sscanf(line, "\nUid: %u %u %u %u", &r, &e, &s, &f) != 4) {
    errno = EACCES;
    return -1;
  }
  if (r != uid && e != uid && s != uid && f != uid)
    return 0;
  if (r != uid || e != uid || s != uid || f != uid) {
    errno = EACCES;
    return -1;
  }
  return 1;
}
static int uid_processes(uid_t uid, int signal_number) {
  DIR *dir = opendir("/proc");
  if (!dir)
    return -1;
  struct dirent *entry;
  int count = 0;
  while ((entry = readdir(dir))) {
    char *end;
    long pid = strtol(entry->d_name, &end, 10);
    if (*end || pid < 2 || pid == getpid())
      continue;
    int match = process_uid((pid_t)pid, uid);
    if (match < 0) {
      int error = errno;
      closedir(dir);
      errno = error;
      return -1;
    }
    if (!match)
      continue;
    int handle = (int)syscall(SYS_pidfd_open, (pid_t)pid, 0);
    if (handle < 0) {
      if (errno == ESRCH)
        continue;
      int error = errno;
      closedir(dir);
      errno = error;
      return -1;
    }
    match = process_uid((pid_t)pid, uid);
    if (match == 1 && ticks((pid_t)pid)) {
      count++;
      if (signal_number &&
          syscall(SYS_pidfd_send_signal, handle, signal_number, NULL, 0) &&
          errno != ESRCH) {
        int error = errno;
        close(handle);
        closedir(dir);
        errno = error;
        return -1;
      }
    }
    int error = errno;
    close(handle);
    errno = error;
    if (match < 0) {
      closedir(dir);
      return -1;
    }
  }
  closedir(dir);
  return count;
}
static void reap(void) {
  int status;
  while (waitpid(-1, &status, WNOHANG) > 0) {
  }
}
static int terminate(uid_t uid) {
  struct timespec pause = {0, 50000000};
  for (unsigned i = 0; i < 40; i++) {
    if (uid_processes(uid, i < 20 ? SIGTERM : SIGKILL) < 0)
      return -1;
    reap();
    if (uid_processes(uid, 0) == 0)
      return 0;
    nanosleep(&pause, NULL);
  }
  errno = EBUSY;
  return -1;
}
static int reserve_uid(int marker, const unsigned char id[16], uid_t *uid) {
  int dir = dir_at(marker, "uid-reservations", 0700, 1), global = -1;
  if (dir < 0)
    return -1;
#ifndef FIRECRAB_WORKSPACE_TEST
  int run = open("/run", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
  if (run >= 0) {
    global = dir_at(run, "firecrab-workspace-users", 0700, 1);
    close(run);
  }
  if (global < 0) {
    close(dir);
    return -1;
  }
#endif
  unsigned seed = n32(id);
  int result = -1;
  for (unsigned i = 0; i < 256; i++) {
    uid_t candidate = 100000U + (seed + i) % 1000000000U;
    if (getpwuid(candidate) || uid_processes(candidate, 0) != 0)
      continue;
    char name[32];
    snprintf(name, sizeof name, "%u", (unsigned)candidate);
    int fd = file_at(dir, name, O_WRONLY | O_CREAT | O_EXCL);
    if (fd < 0) {
      if (errno == EEXIST)
        continue;
      break;
    }
    int failed = io_all(fd, (void *)id, 16, 1) || fsync(fd) || fsync(dir);
    int error = errno;
    close(fd);
    errno = error;
    if (failed)
      break;
    if (global >= 0) {
      fd = file_at(global, name, O_WRONLY | O_CREAT | O_EXCL);
      if (fd < 0) {
        if (errno == EEXIST)
          continue;
        break;
      }
      failed = io_all(fd, (void *)id, 16, 1) || fsync(fd) || fsync(global);
      error = errno;
      close(fd);
      errno = error;
      if (failed)
        break;
    }
    *uid = candidate;
    result = 0;
    break;
  }
  if (global >= 0)
    close(global);
  close(dir);
  if (result)
    errno = EBUSY;
  return result;
}
static int protect_marker(int root, int marker) {
#ifndef FIRECRAB_WORKSPACE_TEST
  FILE *f = fopen("/proc/self/mountinfo", "r");
  if (!f)
    return -1;
  char *line = NULL;
  size_t capacity = 0;
  int mounted = 0;
  while (getline(&line, &capacity, f) > 0) {
    char path[1024];
    if (sscanf(line, "%*u %*u %*s %*s %1023s", path) == 1 &&
        !strcmp(path, "/workspace/shared/.firecrab-workspace")) {
      mounted = 1;
      break;
    }
  }
  free(line);
  fclose(f);
  if (!mounted) {
    char source[64];
    snprintf(source, sizeof source, "/proc/self/fd/%d", marker);
    if (mount(source, "/workspace/shared/.firecrab-workspace", NULL, MS_BIND,
              NULL))
      return -1;
  }
  struct stat a, b;
  if (fstat(marker, &a) ||
      fstatat(root, ".firecrab-workspace", &b, AT_SYMLINK_NOFOLLOW) ||
      a.st_ino != b.st_ino || a.st_dev != b.st_dev) {
    errno = EACCES;
    return -1;
  }
#else
  (void)root;
#endif
  return fchmod(marker, 0711);
}
static int share_files(int root, gid_t gid, unsigned depth, unsigned *count,
                       dev_t device) {
  if (depth > 32) {
    errno = EFBIG;
    return -1;
  }
  DIR *d = fdopendir(dup(root));
  if (!d)
    return -1;
  struct dirent *e;
  int result = -1;
  errno = 0;
  while ((e = readdir(d))) {
    if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..") ||
        !strncmp(e->d_name, ".firecrab-", 10) ||
        !strcmp(e->d_name, "lost+found"))
      continue;
    if (++*count > 16384) {
      errno = EFBIG;
      goto out;
    }
    struct stat s;
    if (fstatat(root, e->d_name, &s, AT_SYMLINK_NOFOLLOW))
      goto out;
    if (s.st_dev != device ||
        (!S_ISDIR(s.st_mode) && !(S_ISREG(s.st_mode) && s.st_nlink == 1)))
      continue;
    int fd = openat(root, e->d_name,
                    O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC |
                        (S_ISDIR(s.st_mode) ? O_DIRECTORY : 0));
    if (fd < 0)
      goto out;
    int failed = S_ISDIR(s.st_mode)
                     ? share_files(fd, gid, depth + 1, count, device)
                     : (fchown(fd, (uid_t)-1, gid) ||
                        fchmod(fd, (s.st_mode & 0770) | 0660));
    int error = errno;
    close(fd);
    errno = error;
    if (failed)
      goto out;
    errno = 0;
  }
  if (errno)
    goto out;
  struct stat own;
  if (fstat(root, &own) || fchown(root, (uid_t)-1, gid) ||
      fchmod(root, (own.st_mode & 0770) | 02770))
    goto out;
  result = 0;
out: {
  int error = errno;
  closedir(d);
  errno = error;
  return result;
}
}
static int group_prepare(int root, int marker, const char *project,
                         gid_t *gid) {
  unsigned hash = 2166136261U;
  for (const char *p = project; *p; p++)
    hash = (hash ^ (unsigned char)*p) * 16777619U;
  *gid = 2000000000U + hash % 100000000U;
  /* These numeric identities are private to the guest; never alter an existing
   * account. */
  if (getgrgid(*gid)) {
    errno = EACCES;
    return -1;
  }
  if (protect_marker(root, marker))
    return -1;
  struct stat s;
  unsigned count = 0;
  if (fstat(root, &s))
    return -1;
  return share_files(root, *gid, 0, &count, s.st_dev);
}
static int socket_path(int dir, char path[108]) {
  return snprintf(path, 108, "/proc/self/fd/%d/control.sock", dir) >= 108 ? -1
                                                                          : 0;
}
static int read_meta(int dir, unsigned char meta[META]) {
  if (read_exact(dir, "state", meta, META) || memcmp(meta, "FCTS", 4) ||
      n32(meta + 4) > 3 || n32(meta + 8) < 100000) {
    errno = ESTALE;
    return -1;
  }
  return 0;
}
static int save_meta(int dir, unsigned char meta[META]) {
  return atomic_record(dir, "state", meta, META);
}
static int same_boot(const unsigned char meta[META]) {
  unsigned char boot[16];
  return !boot_id(boot) && !memcmp(boot, meta + 40, 16);
}
static int live_server(const unsigned char meta[META]) {
  return same_boot(meta) && n32(meta + 4) == 1 &&
         ticks((pid_t)n32(meta + 16)) == n64(meta + 24);
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
  uint64_t end = n64(meta + 64);
  if (UINT64_MAX - end < n) {
    errno = EFBIG;
    return -1;
  }
  char name[40];
  snprintf(name, sizeof name, "%020llu.chunk", (unsigned long long)end);
  int fd = file_at(chunks, name, O_WRONLY | O_CREAT | O_EXCL);
  if (fd < 0)
    return -1;
  int failed = io_all(fd, data, n, 1) || fsync(fd) || fsync(chunks);
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
  p64(meta + 56, begin);
  p64(meta + 64, end);
  if (begin)
    p32(meta + 76, n32(meta + 76) | 1);
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
  uint64_t begin = n64(meta + 56), end = n64(meta + 64);
  if (cursor > end) {
    errno = EINVAL;
    return rejected(client);
  }
  if (cursor < begin)
    cursor = begin;
  unsigned char result[META + 8 + CHUNK];
  memcpy(result, meta, META);
  p64(result + META, cursor);
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
    int fd = file_at(chunks, name, O_RDONLY);
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
static void close_except(int a, int b, int c, int d, int e, int f) {
  DIR *dir = opendir("/proc/self/fd");
  if (!dir)
    _exit(77);
  int own = dirfd(dir);
  struct dirent *entry;
  while ((entry = readdir(dir))) {
    char *end;
    long fd = strtol(entry->d_name, &end, 10);
    if (!*end && fd >= 3 && fd != own && fd != a && fd != b && fd != c &&
        fd != d && fd != e && fd != f)
      close((int)fd);
  }
  closedir(dir);
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
  close_except(-1, -1, -1, -1, -1, -1);
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
  if (read_exact(dir, "session", expected, 16) ||
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
    p32(meta + 80, size.ws_row);
    p32(meta + 84, size.ws_col);
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
  int home = dir_at(dir, "home", 0700, 1),
      chunks = dir_at(dir, "chunks", 0700, 1);
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
  p32(meta + 80, size.ws_row);
  p32(meta + 84, size.ws_col);
  close_except(dir, cwd, home, master, server, chunks);
  pid_t child = fork();
  if (child < 0)
    _exit(77);
  if (!child)
    shell(master, cwd, home, (uid_t)n32(meta + 8), (gid_t)n32(meta + 12),
          home_path);
  close(cwd);
  close(home);
  p32(meta + 4, 1);
  p32(meta + 16, (uint32_t)getpid());
  p32(meta + 20, (uint32_t)child);
  p64(meta + 24, ticks(getpid()));
  p64(meta + 32, ticks(child));
  if (!n64(meta + 24) || !n64(meta + 32) || save_meta(dir, meta)) {
    terminate((uid_t)n32(meta + 8));
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
  if (terminate((uid_t)n32(meta + 8)))
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
  p32(meta + 4, failed ? 3 : 2);
  p32(meta + 72, (exited || stop) ? (uint32_t)exit_code : UINT32_MAX);
  if (failed)
    p32(meta + 76, n32(meta + 76) | 2);
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
  if (n32(meta + 4) == 0)
    return response(STDOUT_FILENO, NULL, 0, 9);
  if (n32(meta + 4) == 1 && live_server(meta)) {
    errno = EBUSY;
    return rejected(STDOUT_FILENO);
  }
  if (n32(meta + 4) == 1) {
    p32(meta + 4, 3);
    p32(meta + 76, same_boot(meta) ? n32(meta + 76) | 2 : n32(meta + 76) & ~2U);
    if (save_meta(dir, meta))
      return rejected(STDOUT_FILENO);
  }
  if (r->action == 5 && same_boot(meta)) {
    if (terminate((uid_t)n32(meta + 8)))
      return rejected(STDOUT_FILENO);
    p32(meta + 76, n32(meta + 76) & ~2U);
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
  terminals = dir_at(marker, "terminals", 0711, r.action == 1);
  if (terminals < 0)
    goto done;
  char id[33];
  for (unsigned i = 0; i < 16; i++)
    snprintf(id + i * 2, 3, "%02x", r.header[8 + i]);
  dir = dir_at(terminals, id, 0711, 0);
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
    if (group_prepare(root, marker, argv[2], &gid) ||
        reserve_uid(marker, r.header + 8, &uid))
      goto done;
    dir = dir_at(terminals, id, 0711, 1);
    if (dir < 0)
      goto done;
    unsigned char meta[META] = {0};
    memcpy(meta, "FCTS", 4);
    p32(meta + 8, uid);
    p32(meta + 12, gid);
    p32(meta + 72, UINT32_MAX);
    if (boot_id(meta + 40) || atomic_record(dir, "request", r.header, HEADER) ||
        atomic_record(dir, "cwd", r.data, r.size) ||
        atomic_record(dir, "session", r.header + 8, 16) || save_meta(dir, meta))
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
      if (!read_meta(dir, meta) && n32(meta + 4) != 0)
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
    if (read_exact(dir, "request", header, HEADER) ||
        memcmp(header, r.header, HEADER) ||
        read_exact(dir, "cwd", cwd_bytes, r.size) ||
        memcmp(cwd_bytes, r.data, r.size)) {
      errno = ESTALE;
      goto done;
    }
    r.action = 2;
    r.size = 0;
    p32(r.header + 4, 2);
    p32(r.header + 36, 0);
  }
  chunks = dir_at(dir, "chunks", 0700, 0);
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
      peer.uid != 0 || peer.pid != (pid_t)n32(meta + 16) ||
      ticks(peer.pid) != n64(meta + 24)) {
    errno = EACCES;
    goto done;
  }
  if (io_all(client, r.header, HEADER, 1) || io_all(client, r.data, r.size, 1))
    goto transport_failed;
  unsigned char header[8], payload[META + 8 + CHUNK];
  if (io_all(client, header, 8, 0))
    goto transport_failed;
  uint32_t size = n32(header + 4);
  if (!memcmp(header, "FCTE", 4) && size >= 1 && size <= 9) {
    result = response(STDOUT_FILENO, NULL, 0, size);
    goto done;
  }
  if (memcmp(header, "FCTR", 4) || size < META + 8 || size > sizeof payload ||
      io_all(client, payload, size, 0)) {
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
