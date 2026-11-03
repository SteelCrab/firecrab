/* SPDX-License-Identifier: Apache-2.0 */
#define _GNU_SOURCE
#include "workspace.h"
#include "workspace_process.h"
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#ifdef __linux__
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/syscall.h>

#define BODY_LIMIT 98304U
#define META_SIZE 72U
#define OUTPUT_LIMIT 65536U
/* State is published before the unprivileged child can execute. IDs never replay. */
struct command {
    unsigned action, size, wall, cpu, memory, limit, argc, envc, input_size;
    unsigned char id[16], payload[BODY_LIMIT], *input;
    char *cwd, *argv[129], *env[65];
};
static int respond(const char *magic, const void *bytes, unsigned size) {
    unsigned char header[8];
    memcpy(header, magic, 4); fcp_p32(header + 4, size);
    return fcp_io_all(1, header, 8, 1) || (bytes && size && fcp_io_all(1, (void *)bytes, size, 1));
}
static int rejected(void) {
    unsigned code = 7;
    switch (errno) {
        case EINVAL: case ENAMETOOLONG: code = 1; break;
        case ENOENT: code = 2; break;
        case ESTALE: case EEXIST: code = 3; break;
        case EBUSY: case EAGAIN: code = 4; break;
        case EACCES: case EPERM: case ELOOP: case ENOTDIR: code = 5; break;
        case EFBIG: case ENOSPC: code = 6; break;
        case ENOTSUP: case ENODEV: code = 8; break;
    }
    return respond("FCXE", NULL, code);
}
static int string_at(struct command *r, unsigned *offset, char **text, unsigned *total) {
    if (*offset > r->size || r->size - *offset < 4) return -1;
    unsigned n = fcp_n32(r->payload + *offset); *offset += 4;
    if (n > r->size - *offset || memchr(r->payload + *offset, 0, n)) return -1;
    *text = strndup((char *)r->payload + *offset, n);
    if (!*text) return -1;
    *offset += n; *total += n;
    return 0;
}
static int read_request(struct command *r) {
    unsigned char header[28], extra;
    if (fcp_io_all(0, header, sizeof header, 0) || memcmp(header, "FCX1", 4)) return -1;
    r->action = fcp_n32(header + 4); r->size = fcp_n32(header + 24);
    memcpy(r->id, header + 8, 16);
    if (r->action < 1 || r->action > 4 || r->size > BODY_LIMIT ||
        !memcmp(r->id, (unsigned char[16]){0}, 16) ||
        (r->action == 4 ? r->size != 0 : r->size < 32) ||
        fcp_io_all(0, r->payload, r->size, 0) || read(0, &extra, 1) != 0) return -1;
    if (r->action == 4) return 0;
    r->wall = fcp_n32(r->payload); r->cpu = fcp_n32(r->payload + 4);
    r->memory = fcp_n32(r->payload + 8); r->limit = fcp_n32(r->payload + 12);
    unsigned cwd_size = fcp_n32(r->payload + 16), offset = 32, total = 0;
    r->argc = fcp_n32(r->payload + 20); r->envc = fcp_n32(r->payload + 24);
    r->input_size = fcp_n32(r->payload + 28);
    if (!r->wall || r->wall > 120 || !r->cpu || r->cpu > 120 || r->memory < 16 || r->memory > 65536 ||
        !r->limit || r->limit > OUTPUT_LIMIT || !r->argc || r->argc > 128 || r->envc > 64 ||
        r->input_size > 65536 || cwd_size > 1024 || cwd_size > r->size - offset ||
        memchr(r->payload + offset, 0, cwd_size)) return -1;
    r->cwd = strndup((char *)r->payload + offset, cwd_size); offset += cwd_size;
    if (!r->cwd) return -1;
    for (unsigned i = 0; i < r->argc; i++)
        if (string_at(r, &offset, &r->argv[i], &total)) return -1;
    if (total > 8192 || !r->argv[0][0] || r->argv[0][0] == '-' || strchr(r->argv[0], '=')) return -1;
    total = 0;
    for (unsigned i = 0; i < r->envc; i++) {
        if (string_at(r, &offset, &r->env[i], &total)) return -1;
        const char *eq = strchr(r->env[i], '=');
        if (!eq || eq == r->env[i] || eq - r->env[i] > 128) return -1;
        for (const char *p = r->env[i]; p != eq; p++)
            if (!((*p >= 'A' && *p <= 'Z') || (*p >= 'a' && *p <= 'z') || *p == '_' ||
                  (p != r->env[i] && *p >= '0' && *p <= '9'))) return -1;
        for (unsigned j = 0; j < i; j++)
            if (!strncmp(r->env[j], r->env[i], (size_t)(eq - r->env[i]) + 1)) return -1;
    }
    if (total > 16384 || offset > r->size || r->size - offset != r->input_size) return -1;
    r->input = r->payload + offset;
    return 0;
}
static int meta_read(int dir, unsigned char m[META_SIZE]) {
    if (fcp_read_exact(dir, "state", m, META_SIZE) || memcmp(m, "FCXS", 4) ||
        fcp_n32(m + 4) > 3 || fcp_n32(m + 8) < 100000 ||
        fcp_n32(m + 64) > OUTPUT_LIMIT || fcp_n32(m + 68) > OUTPUT_LIMIT) {
        errno = ESTALE; return -1;
    }
    return 0;
}
static int supervisor_live(int dir, int current_boot) {
    unsigned char proof[12];
    if (!current_boot || fcp_read_exact(dir, "supervisor", proof, sizeof proof)) return 0;
    return fcp_n32(proof) > 1 && fcp_ticks((pid_t)fcp_n32(proof)) == fcp_n64(proof + 4);
}
static int meta_save(int dir, const unsigned char *m) { return fcp_atomic_record(dir, "state", m, META_SIZE); }
static int output_reply(int dir, const unsigned char *m) {
    unsigned out = fcp_n32(m + 64), err = fcp_n32(m + 68);
    unsigned char *body = malloc(META_SIZE + out + err);
    if (!body) return -1;
    memcpy(body, m, META_SIZE);
    int result = -1;
    const char *names[2] = {"stdout", "stderr"}; unsigned sizes[2] = {out, err}, at = META_SIZE;
    for (unsigned i = 0; i < 2; i++) {
        if (sizes[i]) {
            int fd = fcp_file_at(dir, names[i], O_RDONLY);
            if (fd < 0) goto done;
            int failed = fcp_io_all(fd, body + at, sizes[i], 0); int error = errno;
            close(fd); errno = error;
            if (failed) goto done;
        }
        at += sizes[i];
    }
    result = respond("FCXR", body, at);
done: free(body); return result;
}
static void child(int cwd, int home, int input, int out, int err, const char *home_path,
                  const struct command *r, uid_t uid, gid_t gid) {
    if (dup2(input, 0) < 0 || dup2(out, 1) < 0 || dup2(err, 2) < 0 ||
        fchdir(cwd) || fchown(home, uid, gid) || fchmod(home, 0700)) _exit(77);
    struct rlimit cpu = {r->cpu, r->cpu}, memory = {(rlim_t)r->memory * 1024 * 1024, (rlim_t)r->memory * 1024 * 1024},
        nofile = {256, 256}, processes = {128, 128}, core = {0, 0}, file = {64 * 1024 * 1024, 64 * 1024 * 1024};
    if (setrlimit(RLIMIT_CPU, &cpu) || setrlimit(RLIMIT_AS, &memory) || setrlimit(RLIMIT_NOFILE, &nofile) ||
        setrlimit(RLIMIT_NPROC, &processes) || setrlimit(RLIMIT_CORE, &core) || setrlimit(RLIMIT_FSIZE, &file) ||
        prctl(PR_SET_KEEPCAPS, 0, 0, 0, 0) || prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) ||
        setgroups(0, NULL) || setresgid(gid, gid, gid) || setresuid(uid, uid, uid)) _exit(77);
    umask(0007); fcp_close_except(-1, -1, -1, -1, -1, -1);
    if (clearenv() || setenv("HOME", home_path, 1) || setenv("PATH", "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin", 1) ||
        setenv("LANG", "C.UTF-8", 1)) _exit(77);
    for (unsigned i = 0; i < r->envc; i++) {
        char *eq = strchr(r->env[i], '='); *eq = 0;
        if (setenv(r->env[i], eq + 1, 1)) _exit(77);
    }
    signal(SIGPIPE, SIG_DFL); signal(SIGHUP, SIG_DFL);
    execvp(r->argv[0], r->argv); _exit(errno == ENOENT ? 127 : 126);
}
static int retain_stream(int fd, int sink, unsigned char *m, unsigned index, unsigned limit) {
    unsigned char bytes[4096]; ssize_t n = read(fd, bytes, sizeof bytes);
    if (n < 0) return errno == EAGAIN || errno == EINTR ? 0 : -1;
    if (!n) return 0;
    unsigned size = fcp_n32(m + 64 + index * 4), keep = (unsigned)n, flags = fcp_n32(m + 60);
    if (keep > limit - size) { keep = limit - size; fcp_p32(m + 60, fcp_n32(m + 60) | (2U << index)); }
    if (keep && (fcp_io_all(sink, bytes, keep, 1) || fsync(sink))) return -1;
    fcp_p32(m + 64 + index * 4, size + keep);
    return keep || flags != fcp_n32(m + 60);
}
static void supervise(int dir, int cwd, int lock, int marker, int root, int start,
                      struct command *r, unsigned char *m, const char *home_path) {
    close(lock); close(marker);
    if (setsid() < 0 || prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0)) _exit(77);
    signal(SIGPIPE, SIG_IGN); signal(SIGHUP, SIG_IGN);
    int null = open("/dev/null", O_RDWR);
    if (null < 0) _exit(77);
    for (int i = 0; i < 3; i++) if (dup2(null, i) < 0) _exit(77);
    if (null > 2) close(null);
    unsigned char byte;
    if (read(start, &byte, 1) != 1) _exit(77);
    close(start);
    int home = fcp_dir_at(dir, "home", 0700, 1), input = fcp_file_at(dir, "stdin", O_RDWR | O_CREAT | O_EXCL),
        sinks[2] = {fcp_file_at(dir, "stdout", O_WRONLY | O_CREAT | O_EXCL), fcp_file_at(dir, "stderr", O_WRONLY | O_CREAT | O_EXCL)},
        out[2], err[2], gate[2];
    if (home < 0 || input < 0 || sinks[0] < 0 || sinks[1] < 0 ||
        fcp_io_all(input, r->input, r->input_size, 1) || fsync(input) || lseek(input, 0, SEEK_SET) < 0 ||
        pipe2(out, O_CLOEXEC) || pipe2(err, O_CLOEXEC) || pipe2(gate, O_CLOEXEC) ||
        fcntl(out[0], F_SETFL, O_NONBLOCK) || fcntl(err[0], F_SETFL, O_NONBLOCK)) _exit(77);
    struct stat stop;
    if (!fstatat(dir, "cancel", &stop, AT_SYMLINK_NOFOLLOW)) {
        fcp_p32(m + 4, 2); fcp_p32(m + 56, 143); fcp_p32(m + 60, 8); meta_save(dir, m); _exit(0);
    }
    if (errno != ENOENT) _exit(77);
    pid_t pid = fork();
    if (pid < 0) _exit(77);
    if (!pid) {
        close(gate[1]);
        if (read(gate[0], &byte, 1) != 1) _exit(77);
        child(cwd, home, input, out[1], err[1], home_path, r, (uid_t)fcp_n32(m + 8), (gid_t)fcp_n32(m + 12));
    }
    close(gate[0]); close(out[1]); close(err[1]); close(cwd); close(home); close(input);
    fcp_p32(m + 4, 1); fcp_p32(m + 16, (uint32_t)getpid()); fcp_p32(m + 20, (uint32_t)pid);
    fcp_p64(m + 24, fcp_ticks(getpid())); fcp_p64(m + 32, fcp_ticks(pid));
    if (!fcp_n64(m + 24) || !fcp_n64(m + 32) || meta_save(dir, m) || fsync(dir) || write(gate[1], "1", 1) != 1) _exit(77);
    close(gate[1]); fcp_close_except(dir, out[0], err[0], sinks[0], sinks[1], root);
    struct timespec begin, now; if (clock_gettime(CLOCK_MONOTONIC, &begin)) _exit(77);
    int failed = 0, exited = 0; unsigned exit_code = UINT32_MAX;
    for (;;) {
        struct pollfd streams[2] = {{.fd=out[0],.events=POLLIN},{.fd=err[0],.events=POLLIN}};
        if (poll(streams, 2, 50) < 0 && errno != EINTR) { failed = 1; break; }
        int changed = 0;
        for (unsigned i = 0; i < 2; i++) {
            int result = retain_stream(streams[i].fd, sinks[i], m, i, r->limit);
            if (result < 0) { failed = 1; break; }
            changed |= result;
        }
        if (failed || (changed && meta_save(dir, m))) { failed = 1; break; }
        int status; pid_t end = waitpid(pid, &status, WNOHANG);
        if (end == pid) { exited = 1; exit_code = WIFEXITED(status) ? (unsigned)WEXITSTATUS(status) : 128U + (unsigned)WTERMSIG(status); break; }
        if (end < 0) { failed = 1; break; }
        if (!fstatat(dir, "cancel", &stop, AT_SYMLINK_NOFOLLOW)) {
            exit_code = 143; fcp_p32(m + 60, fcp_n32(m + 60) | 8); break;
        }
        if (errno != ENOENT || clock_gettime(CLOCK_MONOTONIC, &now)) { failed = 1; break; }
        if (now.tv_sec > begin.tv_sec + r->wall || (now.tv_sec == begin.tv_sec + r->wall && now.tv_nsec >= begin.tv_nsec)) {
            exit_code = 124; fcp_p32(m + 60, fcp_n32(m + 60) | 16); break;
        }
    }
    if (fcp_terminate((uid_t)fcp_n32(m + 8))) failed = 1;
    for (unsigned n = 0; n < 64; n++) {
        int changed = 0;
        for (unsigned i = 0; i < 2; i++) {
            int result = retain_stream(i ? err[0] : out[0], sinks[i], m, i, r->limit);
            if (result < 0) failed = 1; else changed |= result;
        }
        if (!changed) break;
    }
    if (syncfs(root)) failed = 1;
    fcp_p32(m + 4, failed ? 3 : 2); fcp_p32(m + 56, exit_code);
    if (failed) fcp_p32(m + 60, fcp_n32(m + 60) | 1);
    if (!exited && exit_code == UINT32_MAX) fcp_p32(m + 4, 3);
    meta_save(dir, m); _exit(failed ? 77 : 0);
}
int firecrab_workspace_command(int argc, char **argv) {
    umask(0077); signal(SIGPIPE, SIG_IGN);
    struct command *r = calloc(1, sizeof *r);
    if (!r) return 1;
    int root=-1,marker=-1,lock=-1,commands=-1,dir=-1,cwd=-1,result=-1;
    if (geteuid() != 0) { errno = EACCES; goto done; }
    if (read_request(r)) { errno = EINVAL; goto done; }
    if (firecrab_workspace_open(argc, argv, &root, &marker, &lock)) goto done;
    int probe = (int)syscall(SYS_pidfd_open, getpid(), 0);
    if (probe < 0) { errno = ENOTSUP; goto done; }
    int supported = syscall(SYS_pidfd_send_signal, probe, 0, NULL, 0) == 0; close(probe);
    if (!supported) { errno = ENOTSUP; goto done; }
    if (r->action == 4) { unsigned char version[4]; fcp_p32(version, 1); result=respond("FCXP",version,4); goto done; }
    commands=fcp_dir_at(marker,"commands",0711,r->action==1||r->action==3); if(commands<0) goto done;
    char id[33]; for(unsigned i=0;i<16;i++) snprintf(id+i*2,3,"%02x",r->id[i]);
    dir=fcp_dir_at(commands,id,0711,0);
    if(dir<0 && errno==ENOENT && (r->action==1 || r->action==3)) {
        DIR *list=fdopendir(dup(commands)); if(!list) goto done;
        unsigned count=0; struct dirent *entry;
        while((entry=readdir(list))) if(strcmp(entry->d_name,".")&&strcmp(entry->d_name,"..")) count++;
        closedir(list); if(count>=4096) { errno=ENOSPC; goto done; }
        if(r->action==1) { cwd=firecrab_workspace_directory(root,r->cwd); if(cwd<0) goto done; }
        uid_t uid; gid_t gid;
        if(fcp_group_prepare(root,marker,argv[2],&gid)||fcp_reserve_uid(marker,r->id,&uid)) goto done;
        dir=fcp_dir_at(commands,id,0711,1); if(dir<0) goto done;
        unsigned char m[META_SIZE]={0}; memcpy(m,"FCXS",4); fcp_p32(m+8,uid);fcp_p32(m+12,gid);fcp_p32(m+56,UINT32_MAX);
        if(r->action==3) { fcp_p32(m+4,2); fcp_p32(m+56,143); fcp_p32(m+60,8); }
        if(fcp_boot_id(m+40)||fcp_atomic_record(dir,"request",r->payload,r->size)||meta_save(dir,m)) goto done;
        if(r->action==3) { result=output_reply(dir,m); goto done; }
        char target[64],base[2048],home[2112];snprintf(target,sizeof target,"/proc/self/fd/%d",dir);
        ssize_t n=readlink(target,base,sizeof base-1); if(n<0) goto done; base[n]=0;
        if(snprintf(home,sizeof home,"%s/home",base)>=(int)sizeof home) {errno=ENAMETOOLONG;goto done;}
        int gate[2]; if(pipe2(gate,O_CLOEXEC)) goto done;
        pid_t pid=fork(); if(pid<0) {close(gate[0]);close(gate[1]);goto done;}
        if(!pid) {close(gate[1]);supervise(dir,cwd,lock,marker,root,gate[0],r,m,home);}
        close(gate[0]); unsigned char proof[12]; fcp_p32(proof,(uint32_t)pid);fcp_p64(proof+4,fcp_ticks(pid));
        int failed=!fcp_n64(proof+4)||fcp_atomic_record(dir,"supervisor",proof,sizeof proof)||write(gate[1],"1",1)!=1;
        close(gate[1]); if(failed) goto done;
        struct timespec pause={0,10000000};
        for(unsigned i=0;i<200;i++) {
            if(!meta_read(dir,m)&&fcp_n32(m+4)!=0) break;
            int status; if(waitpid(pid,&status,WNOHANG)==pid) break;
            nanosleep(&pause,NULL);
        }
    }
    if(dir<0) goto done;
    unsigned char original[BODY_LIMIT],m[META_SIZE];
    if(fcp_read_exact(dir,"request",original,r->size)||memcmp(original,r->payload,r->size)||meta_read(dir,m)) {errno=ESTALE;goto done;}
    unsigned char boot[16];
    if(fcp_boot_id(boot)) goto done;
    int current_boot = !memcmp(boot,m+40,16);
    if(r->action==3 && fcp_atomic_record(dir,"cancel","1",1)) goto done;
    if(fcp_n32(m+4)<2 && !supervisor_live(dir,current_boot)) {
        fcp_p32(m+4,3);fcp_p32(m+60,current_boot?fcp_n32(m+60)|1:fcp_n32(m+60)&~1U);
        if(meta_save(dir,m)) goto done;
    }
    if(r->action==3 && fcp_n32(m+4)==3) {
        if(current_boot && fcp_terminate((uid_t)fcp_n32(m+8))) goto done;
        if(syncfs(root)) goto done;
        fcp_p32(m+60,fcp_n32(m+60)&~1U);if(meta_save(dir,m)) goto done;
    }
    result=output_reply(dir,m);
done: {
    int error=errno;int fds[]={cwd,dir,commands,lock,marker,root};
    for(unsigned i=0;i<sizeof fds/sizeof fds[0];i++)if(fds[i]>=0)close(fds[i]);
    for(unsigned i=0;i<128;i++) free(r->argv[i]);
    for(unsigned i=0;i<64;i++) free(r->env[i]);
    free(r->cwd);free(r);
    errno=error;return result<0?rejected():result;
}
}
#else
int firecrab_workspace_command(int argc,char **argv){(void)argc;(void)argv;return 77;}
#endif
