/* SPDX-License-Identifier: GPL-3.0-or-later */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

/* One-shot disk-latency injection for a disposable editor document only. */
int fsync(int fd) {
    int (*real_fsync)(int) = dlsym(RTLD_NEXT, "fsync");
    const char *prefix = getenv("FM_ARCH_PROJECT");
    const char *flag = getenv("FM_ARCH_DELAY_FLAG");
    const char *entered = getenv("FM_ARCH_ENTERED");
    char proc[64], path[4096];
    snprintf(proc, sizeof(proc), "/proc/self/fd/%d", fd);
    ssize_t n = readlink(proc, path, sizeof(path)-1);
    if (n > 0) path[n] = 0;
    if (prefix && flag && entered && n > 0 &&
        !strncmp(path, prefix, strlen(prefix)) && !unlink(flag)) {
        int log = open(entered, O_WRONLY|O_CREAT|O_TRUNC, 0600);
        if (log >= 0) {
            dprintf(log, "pid=%d tid=%ld path=%s delay_ms=3000\n", getpid(), syscall(SYS_gettid), path);
            close(log);
        }
        struct timespec wait = {.tv_sec=3};
        nanosleep(&wait, NULL);
    }
    return real_fsync(fd);
}
