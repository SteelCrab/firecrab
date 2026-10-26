/* SPDX-License-Identifier: Apache-2.0 */
#define _GNU_SOURCE
#include "user.h"
#include <errno.h>
#include <grp.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>
#include <time.h>

static int lock_descriptor(const char *descriptor, const char *seconds) {
    if (!descriptor || !*descriptor || !seconds || !*seconds) return 77;
    for (const char *p = descriptor; *p; ++p) if (*p < '0' || *p > '9') return 77;
    for (const char *p = seconds; *p; ++p) if (*p < '0' || *p > '9') return 77;
    char *end;
    errno = 0;
    unsigned long fd = strtoul(descriptor, &end, 10);
    if (errno || *end || fd < 3 || fd > 1024) return 77;
    errno = 0;
    unsigned long timeout = strtoul(seconds, &end, 10);
    if (errno || *end || timeout > 10) return 77;
    struct timespec start, current;
    if (clock_gettime(CLOCK_MONOTONIC, &start)) return 77;
    for (;;) {
        if (!flock((int)fd, LOCK_EX | LOCK_NB)) return 0;
        if (errno != EWOULDBLOCK && errno != EAGAIN && errno != EINTR) return 77;
        if (clock_gettime(CLOCK_MONOTONIC, &current)) return 77;
        if (current.tv_sec > start.tv_sec + (time_t)timeout ||
            (current.tv_sec == start.tv_sec + (time_t)timeout && current.tv_nsec >= start.tv_nsec)) return 81;
        struct timespec pause = {.tv_sec = 0, .tv_nsec = 20000000};
        nanosleep(&pause, NULL);
    }
}

static int identity(const char *text, unsigned long *value) {
    if (!text || !*text) return -1;
    for (const char *p = text; *p; ++p) if (*p < '0' || *p > '9') return -1;
    errno = 0;
    char *end;
    *value = strtoul(text, &end, 10);
    return errno || *end || !*value || *value >= UINT32_MAX ? -1 : 0;
}

/* This executable is never setuid. Only the authenticated root supervisor can use it. */
int firecrab_sandbox_user(int argc, char **argv) {
    if (geteuid() != 0) {
        fputs("sandbox_user_requires_root_supervisor\n", stderr);
        return 77;
    }
    if (argc == 2 && !strcmp(argv[1], "--sandbox-probe")) {
        return prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) < 0 ? 77 : 0;
    }
    if (argc == 4 && !strcmp(argv[1], "--sandbox-lock")) {
        return lock_descriptor(argv[2], argv[3]);
    }
    if (argc < 8 || strcmp(argv[1], "--reuid") || strcmp(argv[3], "--regid") ||
        strcmp(argv[5], "--clear-groups") || strcmp(argv[6], "--no-new-privs") ||
        !argv[7][0]) {
        fputs("unsupported_sandbox_user_contract\n", stderr);
        return 77;
    }
    unsigned long uid, gid;
    if (identity(argv[2], &uid) || identity(argv[4], &gid) ||
        (unsigned long)(uid_t)uid != uid || (unsigned long)(gid_t)gid != gid) {
        fputs("sandbox_user_requires_non_root_identity\n", stderr);
        return 77;
    }
    umask(0077);
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) || setgroups(0, NULL) ||
        setresgid((gid_t)gid, (gid_t)gid, (gid_t)gid) ||
        setresuid((uid_t)uid, (uid_t)uid, (uid_t)uid)) {
        perror("sandbox_user_drop_identity");
        return 77;
    }
    if (getuid() != (uid_t)uid || geteuid() != (uid_t)uid ||
        getgid() != (gid_t)gid || getegid() != (gid_t)gid ||
        prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) != 1) {
        fputs("sandbox_user_identity_not_confirmed\n", stderr);
        return 77;
    }
    execvp(argv[7], &argv[7]);
    int error = errno;
    perror("sandbox_user_exec");
    return error == ENOENT ? 127 : 126;
}
