/*
 * shm_copy.h — the row-copy logic used by XShmGetImage's proxy path,
 * pulled out so it can be exercised by a standalone test without needing
 * dlsym/X server plumbing (see test_shm_copy.c).
 */
#ifndef ZCALL_BRIDGE_SHM_COPY_H
#define ZCALL_BRIDGE_SHM_COPY_H

#include <string.h>
#include <X11/Xlib.h>

/* Copies min(im->height, image->height) rows of min(bytes_per_line) bytes
 * each from im into image. Both height and bytes_per_line are clamped so a
 * source image larger than the destination buffer in either dimension
 * cannot write past the end of image->data. */
static inline void shm_copy_image(XImage *image, const XImage *im) {
    size_t copy = im->bytes_per_line < image->bytes_per_line
                      ? (size_t)im->bytes_per_line
                      : (size_t)image->bytes_per_line;
    unsigned int rows = (unsigned int)im->height < (unsigned int)image->height
                             ? (unsigned int)im->height
                             : (unsigned int)image->height;
    for (unsigned int r = 0; r < rows; r++)
        memcpy(image->data + (size_t)r * image->bytes_per_line,
               im->data + (size_t)r * im->bytes_per_line, copy);
}

#endif
