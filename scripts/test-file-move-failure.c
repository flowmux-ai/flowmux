/* SPDX-License-Identifier: GPL-3.0-or-later */
/* Test-only source-deletion boundary. Never intercept outside the fixture path.
 * macOS: cc -dynamiclib scripts/test-file-move-failure.c -o /tmp/fm-move.dylib
 * Load only into the native smoke binary with DYLD_INSERT_LIBRARIES. */
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int fixture_unlink(const char *path) {
    const char *source = getenv("FLOWMUX_FILE_MOVE_SMOKE_SOURCE");
    const char *entered = getenv("FLOWMUX_FILE_MOVE_SMOKE_ENTERED");
    const char *release = getenv("FLOWMUX_FILE_MOVE_SMOKE_RELEASE");
    const char *mode = getenv("FLOWMUX_FILE_MOVE_SMOKE_MODE");
    if (!source || !entered || !release || !mode ||
        strncmp(path, source, strlen(source)) || path[strlen(source)] != '/') {
        return unlink(path);
    }
    if (!strcmp(mode, "error") && access(entered, F_OK) == 0) {
        errno = EACCES;
        return -1;
    }
    int result = unlink(path);
    if (result != 0) return result;
    int marker = open(entered, O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (marker < 0) return result;
    close(marker);
    if (!strcmp(mode, "cancel")) {
        for (int i = 0; i < 10000; ++i) {
            if (access(release, F_OK) == 0) return result;
            usleep(1000);
        }
        abort(); /* A missing UI cancellation must fail, not silently pass. */
    }
    return result;
}

__attribute__((used)) static struct {
    const void *replacement;
    const void *original;
} interpose_unlink __attribute__((section("__DATA,__interpose"))) = {
    (const void *)fixture_unlink, (const void *)unlink
};
