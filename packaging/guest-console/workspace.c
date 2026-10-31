/* SPDX-License-Identifier: Apache-2.0 */
#define _GNU_SOURCE
#include "workspace.h"
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>
#ifdef __linux__
#include <mntent.h>
#endif

#define FILE_LIMIT (1024U * 1024U)
#define PATH_LIMIT 1024U
#define FRAME_HEADER 36U
#define FRAME_LIMIT (FRAME_HEADER + PATH_LIMIT + 2U * FILE_LIMIT)
#define JOURNAL_LIMIT (256U * 1024U * 1024U)
#define ENTRY_LIMIT 4096U
/* A journal receipt is the authority after a transport or controller failure. */
static int uncertain_write;

static uint32_t number(const unsigned char *p) {
    return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) |
           ((uint32_t)p[2] << 8) | p[3];
}
static void put_number(unsigned char *p, uint32_t value) {
    for (int i = 3; i >= 0; i--) { p[i] = (unsigned char)value; value >>= 8; }
}
static int transfer(int fd, void *buffer, size_t length, int writing) {
    unsigned char *p = buffer;
    while (length) {
        ssize_t n = writing ? write(fd, p, length) : read(fd, p, length);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) { if (!n) errno = EIO; return -1; }
        p += n; length -= (size_t)n;
    }
    return 0;
}
static int reply(const char *magic, const void *bytes, uint32_t length) {
    unsigned char header[8];
    memcpy(header, magic, 4); put_number(header + 4, length);
    if (transfer(STDOUT_FILENO, header, sizeof header, 1)) return 1;
    return bytes && length && transfer(STDOUT_FILENO, (void *)bytes, length, 1) ? 1 : 0;
}
static int error_reply(void) {
    unsigned code = 7;
    switch (errno) {
        case EINVAL: case ENAMETOOLONG: code = 1; break;
        case ENOENT: code = 2; break;
        case EEXIST: case ESTALE: code = 3; break;
        case EAGAIN: case EBUSY: code = 4; break;
        case EACCES: case EPERM: case ELOOP: case ENOTDIR: code = 5; break;
        case EFBIG: case ENOSPC: code = 6; break;
        case ENODEV: case ENOTSUP: code = 8; break;
    }
    if (uncertain_write) code = 9;
    return reply("FCWE", NULL, code);
}
static int hexadecimal(const char *text, size_t length) {
    if (strlen(text) != length) return 0;
    for (size_t i = 0; i < length; i++)
        if (!((text[i] >= '0' && text[i] <= '9') || (text[i] >= 'a' && text[i] <= 'f'))) return 0;
    return 1;
}
static int uuid_bytes(const char *text, unsigned char result[16]) {
    if (strlen(text) != 36) return -1;
    char compact[33]; size_t index = 0;
    for (size_t i = 0; i < 36; i++) {
        if (i == 8 || i == 13 || i == 18 || i == 23) {
            if (text[i] != '-') return -1;
        } else compact[index++] = text[i];
    }
    compact[32] = 0;
    if (!hexadecimal(compact, 32)) return -1;
    for (size_t i = 0; i < 16; i++) {
        unsigned n;
        if (sscanf(compact + i * 2, "%2x", &n) != 1) return -1;
        result[i] = (unsigned char)n;
    }
    return 0;
}
static int component(const char *name) {
    size_t n = strlen(name);
    if (!n || n > 255 || !strcmp(name, ".") || !strcmp(name, "..") ||
        !strncmp(name, ".firecrab-", 10) || !strcmp(name, "lost+found")) return 0;
    for (size_t i = 0; i < n; i++)
        if ((unsigned char)name[i] < 32 || (unsigned char)name[i] == 127 || name[i] == '\\') return 0;
    return 1;
}
static int confined_dir(int parent, const char *name, int create, dev_t device) {
    int fd = openat(parent, name, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    if (fd < 0 && errno == ENOENT && create) {
        struct stat owner;
        if (fstat(parent, &owner) || mkdirat(parent, name, 0755)) return -1;
        fd = openat(parent, name, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
        if (fd >= 0 && (fchown(fd, owner.st_uid, owner.st_gid) ||
                        fchmod(fd, owner.st_mode & 02777) || fsync(parent))) {
            int error = errno; close(fd); errno = error; return -1;
        }
    }
    if (fd < 0) return -1;
    struct stat info;
    if (fstat(fd, &info) || info.st_dev != device) {
        close(fd); errno = EACCES; return -1;
    }
    return fd;
}
static int parent_dir(int root, char *path, int create, dev_t device, char **name) {
    int fd = dup(root); unsigned depth = 0;
    if (fd < 0) return -1;
    char *part = path;
    for (;;) {
        char *slash = strchr(part, '/');
        if (slash) *slash = 0;
        if (++depth > 32 || !component(part)) { close(fd); errno = EINVAL; return -1; }
        if (!slash) { *name = part; return fd; }
        int next = confined_dir(fd, part, create, device);
        int error = errno; close(fd);
        if (next < 0) { errno = error; return -1; }
        fd = next; part = slash + 1;
    }
}
static int regular(int fd, dev_t device, int private_file) {
    struct stat info;
    if (fstat(fd, &info)) return -1;
    if (!S_ISREG(info.st_mode) || info.st_nlink != 1 || info.st_dev != device ||
        (private_file && (info.st_uid != geteuid() || (info.st_mode & 0077)))) {
        errno = EACCES; return -1;
    }
    return 0;
}
static int read_file(int parent, const char *name, dev_t device, unsigned char **data,
                     uint32_t *length, uint32_t limit, int private_file) {
    int fd = openat(parent, name, O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC);
    if (fd < 0) return -1;
    if (regular(fd, device, private_file)) { int e = errno; close(fd); errno = e; return -1; }
    unsigned char *bytes = malloc((size_t)limit + 1);
    if (!bytes) { close(fd); errno = ENOMEM; return -1; }
    size_t count = 0;
    while (count <= limit) {
        ssize_t n = read(fd, bytes + count, (size_t)limit + 1 - count);
        if (n < 0 && errno == EINTR) continue;
        if (n < 0) { int e = errno; free(bytes); close(fd); errno = e; return -1; }
        if (!n) break;
        count += (size_t)n;
    }
    close(fd);
    if (count > limit) { free(bytes); errno = EFBIG; return -1; }
    *data = bytes; *length = (uint32_t)count;
    return 0;
}
static int volume_matches(int fd, const unsigned char volume[16]) {
#if defined(__linux__) && !defined(FIRECRAB_WORKSPACE_TEST)
    struct stat root;
    if (fstat(fd, &root)) return -1;
    FILE *mounts = setmntent("/proc/mounts", "r");
    if (!mounts) return -1;
    struct mntent *entry; int result = -1;
    while ((entry = getmntent(mounts))) {
        if (strcmp(entry->mnt_dir, "/workspace/shared") || strcmp(entry->mnt_type, "ext4")) continue;
        int disk = open(entry->mnt_fsname, O_RDONLY | O_NONBLOCK | O_CLOEXEC);
        if (disk < 0) break;
        struct stat info; unsigned char uuid[16], magic[2];
        if (!fstat(disk, &info) && S_ISBLK(info.st_mode) && info.st_rdev == root.st_dev &&
            pread(disk, uuid, 16, 1024 + 104) == 16 &&
            pread(disk, magic, 2, 1024 + 56) == 2 && magic[0] == 0x53 && magic[1] == 0xef &&
            !memcmp(uuid, volume, 16)) result = 0;
        close(disk); break;
    }
    endmntent(mounts);
    if (result) errno = ENODEV;
    return result;
#elif defined(FIRECRAB_WORKSPACE_TEST)
    (void)fd; (void)volume; return 0;
#else
    (void)fd; (void)volume; errno = ENOTSUP; return -1;
#endif
}
static int private_dir(int parent, const char *name, dev_t device, int create) {
    int fd = confined_dir(parent, name, create, device);
    if (fd < 0) return -1;
    struct stat info;
    if (fstat(fd, &info) || info.st_uid != geteuid() || (info.st_mode & 0022)) {
        close(fd); errno = EACCES; return -1;
    }
    if (create && fchmod(fd, 0700)) { int e = errno; close(fd); errno = e; return -1; }
    return fd;
}
static int lock_files(int marker, dev_t device) {
    int fd = openat(marker, "files.lock", O_WRONLY | O_CREAT | O_NOFOLLOW | O_CLOEXEC, 0600);
    if (fd < 0) return -1;
    if (regular(fd, device, 1)) { int e = errno; close(fd); errno = e; return -1; }
    struct timespec start, now, pause = {0, 10000000};
    if (clock_gettime(CLOCK_MONOTONIC, &start)) { close(fd); return -1; }
    while (flock(fd, LOCK_EX | LOCK_NB)) {
        if (errno != EAGAIN && errno != EWOULDBLOCK && errno != EINTR) { int e=errno; close(fd); errno=e; return -1; }
        if (clock_gettime(CLOCK_MONOTONIC, &now)) { close(fd); return -1; }
        if (now.tv_sec - start.tv_sec >= 2) { close(fd); errno = EBUSY; return -1; }
        nanosleep(&pause, NULL);
    }
    return fd;
}
static int list_files(int directory, dev_t device, unsigned char **result, uint32_t *length) {
    DIR *dir = fdopendir(dup(directory));
    if (!dir) return -1;
    unsigned char *bytes = malloc(2U * FILE_LIMIT);
    if (!bytes) { closedir(dir); return -1; }
    size_t count = 0, used = 0; struct dirent *entry;
    errno = 0;
    while ((entry = readdir(dir))) {
        if (++count > ENTRY_LIMIT) { errno = EFBIG; goto fail; }
        if (!component(entry->d_name)) continue;
        struct stat info;
        if (fstatat(directory, entry->d_name, &info, AT_SYMLINK_NOFOLLOW)) {
            if (errno == ENOENT) { errno = 0; continue; }
            goto fail;
        }
        if (info.st_dev != device || (!S_ISDIR(info.st_mode) &&
            !(S_ISREG(info.st_mode) && info.st_nlink == 1))) continue;
        size_t n = strlen(entry->d_name);
        if (used + n + 5 > 2U * FILE_LIMIT) { errno = EFBIG; goto fail; }
        bytes[used++] = S_ISDIR(info.st_mode) ? 'd' : 'f';
        put_number(bytes + used, (uint32_t)n); used += 4;
        memcpy(bytes + used, entry->d_name, n); used += n;
        errno = 0;
    }
    if (errno) goto fail;
    closedir(dir); *result = bytes; *length = (uint32_t)used; return 0;
fail:
    { int e = errno; closedir(dir); free(bytes); errno = e; return -1; }
}
static int journal_room(int fd) {
    DIR *dir = fdopendir(dup(fd)); if (!dir) return -1;
    uint64_t size = 0; unsigned count = 0; struct dirent *entry;
    errno = 0;
    while ((entry = readdir(dir))) {
        if (!strcmp(entry->d_name, ".") || !strcmp(entry->d_name, "..")) continue;
        struct stat info;
        if (fstatat(fd, entry->d_name, &info, AT_SYMLINK_NOFOLLOW)) goto fail;
        if (!S_ISREG(info.st_mode) || info.st_size < 0) { errno = EACCES; goto fail; }
        size += (uint64_t)info.st_size;
        if (++count >= ENTRY_LIMIT || size + FRAME_LIMIT > JOURNAL_LIMIT) { errno = ENOSPC; goto fail; }
        errno = 0;
    }
    if (errno) goto fail;
    closedir(dir); return 0;
fail:
    { int e = errno; closedir(dir); errno = e; return -1; }
}
static int same_record(int journal, const char *name, dev_t device,
                       const unsigned char *frame, uint32_t length) {
    unsigned char *bytes = NULL; uint32_t size = 0;
    if (read_file(journal, name, device, &bytes, &size, FRAME_LIMIT, 1)) return -1;
    int matches = size == length && !memcmp(frame, bytes, size);
    free(bytes);
    if (!matches) { errno = ESTALE; return -1; }
    return 0;
}
static int write_record(int journal, const char *name, const void *frame, uint32_t length) {
    int fd = openat(journal, name, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600);
    if (fd < 0) return -1;
    int result = transfer(fd, (void *)frame, length, 1) || fsync(fd);
    int error = errno; close(fd);
    if (result) { errno = error; return -1; }
    return fsync(journal);
}
static int replace_file(int parent, const char *name, const char *temporary,
                         const unsigned char *bytes, uint32_t length) {
    struct stat owner;
    if (fstatat(parent, name, &owner, AT_SYMLINK_NOFOLLOW)) {
        if (errno != ENOENT || fstat(parent, &owner)) return -1;
        owner.st_mode = (owner.st_mode & 02000) ? 0660 : 0600;
    } else if (!S_ISREG(owner.st_mode) || owner.st_nlink != 1) { errno = EACCES; return -1; }
    int fd = openat(parent, temporary, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600);
    if (fd < 0) return -1;
    int result = transfer(fd, (void *)bytes, length, 1) ||
                 fchown(fd, owner.st_uid, owner.st_gid) || fchmod(fd, owner.st_mode & 0777) || fsync(fd);
    int error = errno; close(fd);
    if (!result && renameat(parent, temporary, parent, name)) { result = -1; error = errno; }
    if (result) { unlinkat(parent, temporary, 0); errno = error; return -1; }
    return fsync(parent);
}
static int write_file(int parent, const char *name, int journal, dev_t device,
                      const unsigned char *frame, uint32_t length, const char *id,
                      const unsigned char *base, uint32_t base_length,
                      const unsigned char *content, uint32_t content_length) {
    char pending[48], done[48], temporary[96];
    snprintf(pending, sizeof pending, "%s.pending", id);
    snprintf(done, sizeof done, "%s.done", id);
    if (!same_record(journal, done, device, frame, length)) return fsync(journal);
    if (errno != ENOENT) return -1;
    int retry = !same_record(journal, pending, device, frame, length);
    if (!retry && errno != ENOENT) return -1;
    uncertain_write = retry;
    unsigned char *current = NULL; uint32_t size = 0;
    int exists = !read_file(parent, name, device, &current, &size, FILE_LIMIT, 0);
    if (!exists && errno != ENOENT) return -1;
    int applied = retry && exists && size == content_length && !memcmp(current, content, size);
    int matches = base_length == UINT32_MAX ? !exists :
                  exists && size == base_length && !memcmp(current, base, size);
    free(current);
    if (!applied && !matches) { errno = ESTALE; return -1; }
    if (!retry && (journal_room(journal) || write_record(journal, pending, frame, length))) return -1;
    uncertain_write = 1;
    unsigned char random[16]; char nonce[33];
    int random_fd = open("/dev/urandom", O_RDONLY | O_CLOEXEC);
    if (random_fd < 0) return -1;
    int random_error = transfer(random_fd, random, sizeof random, 0); int error = errno;
    close(random_fd);
    if (random_error) { errno = error; return -1; }
    for (unsigned i = 0; i < 16; i++) snprintf(nonce + i * 2, 3, "%02x", random[i]);
    snprintf(temporary, sizeof temporary, ".firecrab-write-%s-%s", id, nonce);
    if (!applied && replace_file(parent, name, temporary, content, content_length)) return -1;
    if (fsync(parent) || renameat(journal, pending, journal, done) || fsync(journal)) return -1;
    return 0;
}
int firecrab_workspace_open(int argc, char **argv, int *root, int *marker, int *lock) {
    const char *root_path = "/workspace/shared";
#ifdef FIRECRAB_WORKSPACE_TEST
    if (argc != 7) { errno = EINVAL; return -1; }
    root_path = argv[6];
#else
    if (argc != 6 || geteuid() != 0) { errno = EACCES; return -1; }
#endif
    unsigned char project[16], database[16], volume[16];
    if (uuid_bytes(argv[2], project) || uuid_bytes(argv[3], database) ||
        uuid_bytes(argv[4], volume) || !hexadecimal(argv[5], 64)) {
        errno = EINVAL; return -1;
    }
    /* The fixed ancestors and opened root are the authority for every descendant. */
#ifdef FIRECRAB_WORKSPACE_TEST
    *root = open(root_path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
#else
    (void)root_path;
    int workspace = open("/workspace", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    if (workspace >= 0) {
        *root = openat(workspace, "shared", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
        close(workspace);
    }
#endif
    if (*root < 0 || volume_matches(*root, volume)) { errno = ENODEV; return -1; }
    struct stat info;
    if (fstat(*root, &info)) return -1;
    *marker = private_dir(*root, ".firecrab-workspace", info.st_dev, 0);
    if (*marker < 0) { errno = ENODEV; return -1; }
    char identity[160];
    int identity_length = snprintf(identity, sizeof identity, "%s:%s:%s", argv[2], argv[3], argv[5]);
    unsigned char *ready = NULL; uint32_t ready_length = 0;
    if (read_file(*marker, "ready", info.st_dev, &ready, &ready_length, sizeof identity, 1)) {
        errno = ENODEV; return -1;
    }
    int matched = ready_length == (uint32_t)identity_length && !memcmp(ready, identity, ready_length);
    free(ready);
    if (!matched) { errno = ENODEV; return -1; }
    *lock = lock_files(*marker, info.st_dev);
    if (*lock < 0) return -1;
    return 0;
}

int firecrab_workspace_files(int argc, char **argv) {
    umask(0077);
    unsigned char header[FRAME_HEADER];
    if (transfer(STDIN_FILENO, header, sizeof header, 0) || memcmp(header, "FCW1", 4)) {
        errno = EINVAL; return error_reply();
    }
    uint32_t action = number(header + 4), path_length = number(header + 8);
    uncertain_write = action == 3;
    uint32_t base_length = number(header + 12), content_length = number(header + 16);
    if (action < 1 || action > 4 || path_length > PATH_LIMIT || content_length > FILE_LIMIT ||
        (base_length > FILE_LIMIT && base_length != UINT32_MAX) ||
        (action != 3 && (base_length || content_length)) ||
        (action == 4 && path_length) || (action != 1 && action != 4 && !path_length)) {
        errno = EINVAL; return error_reply();
    }
    uint32_t base_size = base_length == UINT32_MAX ? 0 : base_length;
    uint32_t length = FRAME_HEADER + path_length + base_size + content_length;
    unsigned char *frame = malloc(length), *output = NULL; uint32_t output_length = 0;
    int root = -1, marker = -1, lock = -1, directory = -1, parent = -1, result = -1;
    if (!frame) return error_reply();
    memcpy(frame, header, sizeof header);
    if (transfer(STDIN_FILENO, frame + FRAME_HEADER, length - FRAME_HEADER, 0)) goto finish;
    unsigned char extra;
    if (read(STDIN_FILENO, &extra, 1) != 0) { errno = EINVAL; goto finish; }
    char path[PATH_LIMIT + 1];
    memcpy(path, frame + FRAME_HEADER, path_length); path[path_length] = 0;
    if (strlen(path) != path_length) { errno = EINVAL; goto finish; }
    if (firecrab_workspace_open(argc, argv, &root, &marker, &lock)) goto finish;
    struct stat info;
    if (fstat(root, &info)) goto finish;
    char id[33];
    for (unsigned i = 0; i < 16; i++) snprintf(id + i * 2, 3, "%02x", header[20 + i]);
    if (action == 3 || action == 4) {
        if (!memcmp(header + 20, (unsigned char[16]){0}, 16)) { errno = EINVAL; goto finish; }
        directory = private_dir(marker, "writes", info.st_dev, action == 3);
        if (directory < 0) goto finish;
    }
    if (action == 4) {
        char done[48]; snprintf(done, sizeof done, "%s.done", id);
        result = read_file(directory, done, info.st_dev, &output, &output_length, FRAME_LIMIT, 1);
        if (!result) result = fsync(directory);
    } else if (action == 1 && !path_length) {
        result = list_files(root, info.st_dev, &output, &output_length);
    } else {
        char *name = NULL;
        parent = parent_dir(root, path, action == 3, info.st_dev, &name);
        if (parent < 0) goto finish;
        if (action == 1) {
            int child = confined_dir(parent, name, 0, info.st_dev);
            if (child >= 0) { result = list_files(child, info.st_dev, &output, &output_length); close(child); }
        } else if (action == 2) {
            result = read_file(parent, name, info.st_dev, &output, &output_length, FILE_LIMIT, 0);
        } else {
            const unsigned char *base = frame + FRAME_HEADER + path_length;
            result = write_file(parent, name, directory, info.st_dev, frame, length, id,
                                base, base_length, base + base_size, content_length);
        }
    }
finish:
    { int error = errno;
      if (parent >= 0) close(parent);
      if (directory >= 0) close(directory);
      if (lock >= 0) close(lock);
      if (marker >= 0) close(marker);
      if (root >= 0) close(root);
      free(frame); errno = error;
      int code = result ? error_reply() : reply("FCWR", output, output_length);
      free(output); return code; }
}

int firecrab_workspace_directory(int root, const char *path) {
    struct stat info;
    if (fstat(root, &info) || strlen(path) > PATH_LIMIT) { errno = EINVAL; return -1; }
    if (!*path) return dup(root);
    char copy[PATH_LIMIT + 1], *name;
    strcpy(copy, path);
    int parent = parent_dir(root, copy, 0, info.st_dev, &name);
    if (parent < 0) return -1;
    int result = confined_dir(parent, name, 0, info.st_dev);
    int error = errno; close(parent); errno = error;
    return result;
}
