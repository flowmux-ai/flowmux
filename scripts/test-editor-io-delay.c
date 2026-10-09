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

/* One-shot disk-latency injection for a disposable editor document only.
 * Covers both fsync (File::sync_all) and fdatasync (File::sync_data) so a
 * change in the editor's atomic-save flush call cannot silently bypass it. */
static void maybe_delay(int fd, const char *syscall_name) {
    const char *prefix = getenv("FM_ARCH_PROJECT");
    const char *flag = getenv("FM_ARCH_DELAY_FLAG");
    const char *entered = getenv("FM_ARCH_ENTERED");
    char proc[64], path[4096];
    snprintf(proc, sizeof(proc), "/proc/self/fd/%d", fd);
    ssize_t n = readlink(proc, path, sizeof(path)-1);
    if (n <= 0) return;
    path[n] = 0;
    if (prefix && flag && entered &&
        !strncmp(path, prefix, strlen(prefix)) && !unlink(flag)) {
        int log = open(entered, O_WRONLY|O_CREAT|O_TRUNC, 0600);
        if (log >= 0) {
            dprintf(log, "pid=%d tid=%ld call=%s path=%s delay_ms=3000\n",
                    getpid(), syscall(SYS_gettid), syscall_name, path);
            close(log);
        }
        struct timespec wait = {.tv_sec=3};
        nanosleep(&wait, NULL);
    }
}

int fsync(int fd) {
    int (*real)(int) = dlsym(RTLD_NEXT, "fsync");
    if (!real) {
        fprintf(stderr, "test-editor-io-delay: dlsym(fsync) failed: %s\n", dlerror());
        abort();
    }
    maybe_delay(fd, "fsync");
    return real(fd);
}

int fdatasync(int fd) {
    int (*real)(int) = dlsym(RTLD_NEXT, "fdatasync");
    if (!real) {
        fprintf(stderr, "test-editor-io-delay: dlsym(fdatasync) failed: %s\n", dlerror());
        abort();
    }
    maybe_delay(fd, "fdatasync");
    return real(fd);
}
