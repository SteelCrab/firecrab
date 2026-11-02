/* SPDX-License-Identifier: Apache-2.0 */
#define _GNU_SOURCE
#include "workspace_process.h"
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <pwd.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#ifdef __linux__
#include <sys/mount.h>
#include <sys/syscall.h>
uint32_t fcp_n32(const unsigned char *p) {
  return (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 8 |
         p[3];
}
uint64_t fcp_n64(const unsigned char *p) {
  return (uint64_t)fcp_n32(p) << 32 | fcp_n32(p + 4);
}
void fcp_p32(unsigned char *p, uint32_t n) {
  for (int i = 3; i >= 0; i--) {
    p[i] = (unsigned char)n;
    n >>= 8;
  }
}
void fcp_p64(unsigned char *p, uint64_t n) {
  fcp_p32(p, (uint32_t)(n >> 32));
  fcp_p32(p + 4, (uint32_t)n);
}
int fcp_io_all(int fd, void *buf, size_t length, int writing) {
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

int fcp_dir_at(int parent, const char *name, mode_t mode, int create) {
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
/* Atomic record publication may unlink an already-open old read snapshot.
 * Zero links is safe for that read; multiple links and all writable aliases fail. */
int fcp_file_at(int dir, const char *name, int flags) {
  int fd = openat(dir, name, flags | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK, 0600);
  struct stat s;
  if (fd >= 0 && (fstat(fd, &s) || !S_ISREG(s.st_mode) || s.st_uid != 0 ||
                  (s.st_nlink != 1 && !((flags & O_ACCMODE) == O_RDONLY && s.st_nlink == 0)) || (s.st_mode & 0077))) {
    close(fd);
    errno = EACCES;
    return -1;
  }
  return fd;
}
int fcp_read_exact(int dir, const char *name, void *bytes, size_t n) {
  int fd = fcp_file_at(dir, name, O_RDONLY);
  if (fd < 0)
    return -1;
  struct stat s;
  int result =
      fstat(fd, &s) || s.st_size != (off_t)n || fcp_io_all(fd, bytes, n, 0);
  int error = errno;
  close(fd);
  errno = error;
  return result ? -1 : 0;
}
int fcp_atomic_record(int dir, const char *name, const void *data,
                         size_t size) {
  char temp[64];
  if (snprintf(temp, sizeof temp, "%s.tmp", name) >= (int)sizeof temp) {
    errno = EINVAL;
    return -1;
  }
  int fd = fcp_file_at(dir, temp, O_WRONLY | O_CREAT | O_TRUNC);
  if (fd < 0)
    return -1;
  int result = fcp_io_all(fd, (void *)data, size, 1) || fsync(fd);
  int error = errno;
  close(fd);
  if (!result && (renameat(dir, temp, dir, name) || fsync(dir))) {
    result = -1;
    error = errno;
  }
  errno = error;
  return result;
}
uint64_t fcp_ticks(pid_t pid) {
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
int fcp_boot_id(unsigned char id[16]) {
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
    if (match == 1 && fcp_ticks((pid_t)pid)) {
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
int fcp_terminate(uid_t uid) {
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
int fcp_reserve_uid(int marker, const unsigned char id[16], uid_t *uid) {
  int dir = fcp_dir_at(marker, "uid-reservations", 0700, 1), global = -1;
  if (dir < 0)
    return -1;
#ifndef FIRECRAB_WORKSPACE_TEST
  int run = open("/run", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
  if (run >= 0) {
    global = fcp_dir_at(run, "firecrab-workspace-users", 0700, 1);
    close(run);
  }
  if (global < 0) {
    close(dir);
    return -1;
  }
#endif
  unsigned seed = fcp_n32(id);
  int result = -1;
  for (unsigned i = 0; i < 256; i++) {
    uid_t candidate = 100000U + (seed + i) % 1000000000U;
    if (getpwuid(candidate) || uid_processes(candidate, 0) != 0)
      continue;
    char name[32];
    snprintf(name, sizeof name, "%u", (unsigned)candidate);
    int fd = fcp_file_at(dir, name, O_WRONLY | O_CREAT | O_EXCL);
    if (fd < 0) {
      if (errno == EEXIST)
        continue;
      break;
    }
    int failed = fcp_io_all(fd, (void *)id, 16, 1) || fsync(fd) || fsync(dir);
    int error = errno;
    close(fd);
    errno = error;
    if (failed)
      break;
    if (global >= 0) {
      fd = fcp_file_at(global, name, O_WRONLY | O_CREAT | O_EXCL);
      if (fd < 0) {
        if (errno == EEXIST)
          continue;
        break;
      }
      failed = fcp_io_all(fd, (void *)id, 16, 1) || fsync(fd) || fsync(global);
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
int fcp_group_prepare(int root, int marker, const char *project,
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

void fcp_close_except(int a, int b, int c, int d, int e, int f) {
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

#endif
