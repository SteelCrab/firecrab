#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/types.h>
#include <unistd.h>

/* Disposable QA app, not a guest agent or alternate execution transport. */
static void hex(const char *name, const unsigned char *bytes, size_t count) {
    printf("%s=", name);
    for (size_t i = 0; i < count; ++i) printf("%02x", bytes[i]);
    putchar('\n');
}

static int report(int argc, char **argv) {
    char cwd[PATH_MAX];
    if (!getcwd(cwd, sizeof(cwd))) return 70;
    printf("uid=%lu\ngid=%lu\ngroups=%d\nnoNewPrivs=%d\n",
           (unsigned long)getuid(), (unsigned long)getgid(), getgroups(0, NULL),
           prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0));
    struct rlimit bound;
    const int resources[] = {RLIMIT_NOFILE, RLIMIT_AS, RLIMIT_CPU};
    const char *names[] = {"nofile", "addressSpace", "cpu"};
    for (size_t i = 0; i < sizeof(resources) / sizeof(resources[0]); ++i) {
        if (getrlimit(resources[i], &bound)) return 70;
        printf("%s=%llu,%llu\n", names[i], (unsigned long long)bound.rlim_cur,
               (unsigned long long)bound.rlim_max);
    }
    hex("cwd", (const unsigned char *)cwd, strlen(cwd));
    for (int i = 0; i < argc; ++i) {
        char name[32];
        snprintf(name, sizeof(name), "argv%d", i);
        hex(name, (const unsigned char *)argv[i], strlen(argv[i]));
    }
    const char *nonce = getenv("QA_NONCE");
    const char *literal = getenv("QA_LITERAL");
    if (!nonce || !literal) return 71;
    hex("nonce", (const unsigned char *)nonce, strlen(nonce));
    hex("environment", (const unsigned char *)literal, strlen(literal));
    printf("providerCredentialPresent=%d\n", getenv("OPENAI_API_KEY") != NULL);
    unsigned char input[65537];
    size_t count = fread(input, 1, sizeof(input), stdin);
    if (ferror(stdin) || count > 65536) return 72;
    hex("stdin", input, count);
    fprintf(stderr, "QA_STDERR:%s\n", nonce);
    return 7;
}

int main(int argc, char **argv) {
    if (argc > 1 && !strcmp(argv[1], "--report")) return report(argc, argv);
    if (argc > 1 && !strcmp(argv[1], "--overflow")) {
        for (;;) {
            if (fputs("stdout-overflow\n", stdout) == EOF ||
                fputs("stderr-overflow\n", stderr) == EOF) return 73;
            fflush(stdout);
            fflush(stderr);
        }
    }
    if (argc > 1 && !strcmp(argv[1], "--memory")) {
        unsigned char *bytes = malloc(64 * 1024 * 1024);
        if (bytes) {
            volatile unsigned char *pages = bytes;
            for (size_t i = 0; i < 64 * 1024 * 1024; i += 4096) pages[i] = 1;
            free(bytes);
            return 74;
        }
        int refused = errno == ENOMEM;
        printf("allocation_refused=%d\n", refused);
        return refused ? 42 : 75;
    }
    if (argc > 1 && !strcmp(argv[1], "--cpu")) {
        volatile unsigned long counter = 0;
        for (;;) ++counter;
    }
    if (argc > 1 && strcmp(argv[1], "--sleep")) return 64;
    printf("QA_PAUSE_READY pid=%lu\n", (unsigned long)getpid());
    fflush(stdout);
    for (;;) pause();
}
