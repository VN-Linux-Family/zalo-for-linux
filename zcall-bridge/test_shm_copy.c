/*
 * test_shm_copy.c — standalone regression test for the CWE-119 heap
 * overflow fix in XShmGetImage's row-copy path (shm_copy.h).
 *
 * Reproduces the reported shape: a source image (`im`, as returned by the
 * bridge display's XGetImage) taller than the destination buffer
 * (`image`, sized by the caller for its own screen). Before the fix, the
 * copy loop iterated by im->height alone and wrote past the end of
 * image->data; the fix clamps to min(im->height, image->height).
 *
 * The destination buffer is placed via mmap immediately before an
 * unmapped (PROT_NONE) guard page, so any write even one byte past its
 * end raises SIGSEGV instead of silently landing in malloc slack space —
 * a portable out-of-bounds-write detector that doesn't depend on ASan
 * (which currently crashes on this host's macOS/Xcode combination for
 * unrelated reasons: an Apple clang/ASan runtime version mismatch).
 *
 * No X server or Display connection is needed — shm_copy_image() only
 * touches the XImage struct fields and the two data buffers.
 *
 * Build & run (macOS/Linux):
 *   cc -g -I/opt/homebrew/include zcall-bridge/test_shm_copy.c \
 *       -o /tmp/test_shm_copy && /tmp/test_shm_copy
 *   (Linux: drop the -I flag, or point it at wherever X11/Xlib.h lives.)
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include "shm_copy.h"

static XImage make_image(int height, int bytes_per_line, char *data) {
    XImage im;
    memset(&im, 0, sizeof(im));
    im.height = height;
    im.bytes_per_line = bytes_per_line;
    im.data = data;
    return im;
}

/* Maps `size` writable bytes immediately followed by an unmapped guard
 * page, and returns a pointer to the start of those `size` bytes (i.e.
 * the buffer's last byte is the last byte before the guard page). */
static char *alloc_guarded(size_t size) {
    long pagesize = sysconf(_SC_PAGESIZE);
    size_t region = (size_t)pagesize * 2;
    char *base = mmap(NULL, region, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANON, -1, 0);
    if (base == MAP_FAILED) {
        perror("mmap");
        exit(2);
    }
    if (mprotect(base + pagesize, (size_t)pagesize, PROT_NONE) != 0) {
        perror("mprotect");
        exit(2);
    }
    return base + pagesize - size;
}

int main(void) {
    /* Destination: what the caller allocated for its own screen. */
    const int dst_height = 10;
    const int dst_bpl = 64;
    char *dst_data = alloc_guarded((size_t)dst_height * dst_bpl);

    /* Source: bigger in both dimensions, as reported in the CWE-119
     * finding — the bridge display's frame can exceed the destination. */
    const int src_height = 40;
    const int src_bpl = 128;
    char *src_data = malloc((size_t)src_height * src_bpl);
    memset(src_data, 0xAB, (size_t)src_height * src_bpl);

    XImage image = make_image(dst_height, dst_bpl, dst_data);
    XImage im = make_image(src_height, src_bpl, src_data);

    /* If this overruns dst_data's bounds by even one byte, it lands in
     * the guard page and the process dies with SIGSEGV right here. */
    shm_copy_image(&image, &im);

    /* Confirm the clamped rows/bytes were actually copied correctly,
     * not just that we survived. */
    for (int r = 0; r < dst_height; r++) {
        for (int c = 0; c < dst_bpl; c++) {
            unsigned char got = (unsigned char)dst_data[r * dst_bpl + c];
            if (got != 0xAB) {
                fprintf(stderr, "row %d byte %d: expected 0xAB, got 0x%02x\n",
                        r, c, got);
                return 1;
            }
        }
    }

    free(src_data);
    printf("OK: shm_copy_image stayed within dst bounds (guard page intact) "
           "and copied the clamped region correctly\n");
    return 0;
}
