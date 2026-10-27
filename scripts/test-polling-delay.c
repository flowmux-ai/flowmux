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

/* Stall only /proc/PID/cwd reads in the explicitly armed test GUI process. */
ssize_t readlink(const char *path, char *buffer, size_t size) {
    ssize_t (*real)(const char *, char *, size_t) = dlsym(RTLD_NEXT, "readlink");
    if (!real) abort();
    const char *flag = getenv("FLOWMUX_TEST_POLL_FLAG");
    const char *entered = getenv("FLOWMUX_TEST_POLL_ENTERED");
    size_t length = strlen(path);
    if (flag && entered && !strncmp(path, "/proc/", 6) && length > 4 &&
        !strcmp(path + length - 4, "/cwd")) {
        char pid[32] = {0};
        int fd = open(flag, O_RDONLY);
        ssize_t count = fd < 0 ? -1 : read(fd, pid, sizeof(pid) - 1);
        if (fd >= 0) close(fd);
        if (count > 0 && atoi(pid) == getpid()) {
            int log = open(entered, O_WRONLY | O_CREAT | O_APPEND, 0600);
            if (log >= 0) {
                dprintf(log, "pid=%d tid=%ld path=%s\n", getpid(), syscall(SYS_gettid), path);
                close(log);
            }
            /* Removing the flag releases every outstanding call promptly. */
            struct timespec wait = {.tv_nsec = 100000000};
            for (int i = 0; i < 300 && access(flag, F_OK) == 0; ++i)
                nanosleep(&wait, NULL);
        }
    }
    return real(path, buffer, size);
}
