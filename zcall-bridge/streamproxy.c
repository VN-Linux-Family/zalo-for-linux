/*
 * streamproxy.c — LD_PRELOAD shim for ZaloCall (wine).
 *
 * 1) SCREEN PROXY — redirects XGetImage / XShmGetImage / xcb_get_image
 *    from the real X display to a bridge display (:99) for Wayland
 *    screen-share support.  Always active.
 *
 * 2) CAMERA FEEDER (RETIRED — dormant fallback). The Ubuntu 24.04 camera
 *    bug was root-caused to the host's i386 GStreamer 1.24 + libv4l 1.26
 *    stack; the app now ships a bundled i386 stack (zcall-bridge/gst-i386)
 *    instead. The feeder below only activates when a v4l2loopback device
 *    named "Zalo Camera Bridge" exists — nothing installs one anymore, so
 *    in practice it never runs and touches nothing.
 *
 * Build:
 *   gcc -m32 -shared -fPIC -O2 streamproxy.c -ldl -lX11 -lxcb -lpthread -o streamproxy.so
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/ioctl.h>
#include <signal.h>
#include <time.h>
#include <unistd.h>
#include <linux/videodev2.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/extensions/XShm.h>
#include <xcb/xcb.h>
#include <xcb/xproto.h>

static FILE *logf = NULL;

static void plog(const char *fmt, ...) {
    if (!logf) {
        const char *p = getenv("ZCALL_PROXY_LOG");
        if (!p) return;
        logf = fopen(p, "a");
        if (!logf) return;
    }
    va_list ap;
    va_start(ap, fmt);
    vfprintf(logf, fmt, ap);
    va_end(ap);
    fflush(logf);
}

/* ==================================================================
 * CAMERA FEEDER — read real camera → write to v4l2loopback
 * ================================================================== */

#define CAM_LOOPBACK_NAME "Zalo Camera Bridge"
#define CAM_WIDTH        640
#define CAM_HEIGHT       480
#define CAM_FPS          30
#define CAM_NUM_BUFS     4

static pthread_t cam_thread;
static volatile int cam_running = 0;
static int cam_loopback_nr = -1;  /* detected at runtime */

/* Find the v4l2loopback device by card name */
static int cam_find_loopback(void) {
    char path[256], name[256];
    for (int i = 0; i < 10; i++) {
        snprintf(path, sizeof(path),
                 "/sys/class/video4linux/video%d/name", i);
        FILE *f = fopen(path, "r");
        if (!f) continue;
        if (fgets(name, sizeof(name), f)) {
            name[strcspn(name, "\n")] = 0;
            if (strcmp(name, CAM_LOOPBACK_NAME) == 0) {
                fclose(f);
                return i;
            }
        }
        fclose(f);
    }
    return -1;
}

/* Find the first real USB camera device (skip loopback) */
static int cam_find_real_device(char *out, size_t outsz) {
    char path[256], line[256];
    for (int i = 0; i < 10; i++) {
        if (i == cam_loopback_nr) continue;
        snprintf(path, sizeof(path),
                 "/sys/class/video4linux/video%d/device/modalias", i);
        FILE *f = fopen(path, "r");
        if (!f) continue;
        if (fgets(line, sizeof(line), f) && strncmp(line, "usb:", 4) == 0) {
            fclose(f);
            snprintf(out, outsz, "/dev/video%d", i);
            return 0;
        }
        fclose(f);
    }
    return -1;
}

static int cam_real_fd = -1;   /* opened in constructor, held for lifetime */
static char cam_real_dev[64];  /* path to real camera device */

static void *cam_feeder_thread(void *arg) {
    (void)arg;
    const char *trigger = getenv("ZCALL_CAM_TRIGGER");
    if (!trigger) trigger = "/tmp/zcall_cam_on";

    char loopback_dev[64];
    snprintf(loopback_dev, sizeof(loopback_dev), "/dev/video%d", cam_loopback_nr);

    /* Open loopback immediately — Wine needs valid formats */
    int loop_fd = open(loopback_dev, O_WRONLY);
    if (loop_fd < 0) {
        plog("streamproxy: cam feeder: open %s failed: %s\n",
             loopback_dev, strerror(errno));
        return NULL;
    }

    /* Set loopback format */
    uint32_t frame_size = CAM_WIDTH * CAM_HEIGHT * 2;
    struct v4l2_format lfmt;
    memset(&lfmt, 0, sizeof(lfmt));
    lfmt.type = V4L2_BUF_TYPE_VIDEO_OUTPUT;
    lfmt.fmt.pix.width = CAM_WIDTH;
    lfmt.fmt.pix.height = CAM_HEIGHT;
    lfmt.fmt.pix.pixelformat = V4L2_PIX_FMT_YUYV;
    lfmt.fmt.pix.sizeimage = frame_size;
    lfmt.fmt.pix.field = V4L2_FIELD_NONE;
    ioctl(loop_fd, VIDIOC_S_FMT, &lfmt);

    /* Black frame */
    uint8_t *black = calloc(1, frame_size);
    if (black) {
        for (uint32_t i = 0; i < frame_size; i += 2) {
            black[i] = 0;
            black[i+1] = 128;
        }
    }

    plog("streamproxy: cam feeder: ready (black frames, trigger=%s)\n", trigger);

    /* Setup camera capture (using pre-opened fd) */
    void *bufs[CAM_NUM_BUFS] = {0};
    uint32_t nbufs = 0;
    uint32_t real_fsize = frame_size;
    int streaming = 0;

    if (cam_real_fd >= 0) {
        /* Nonblocking capture: a stuck DQBUF must never stall the trigger
         * poll (e.g. camera unplugged mid-call). */
        int fl = fcntl(cam_real_fd, F_GETFL, 0);
        if (fl >= 0) fcntl(cam_real_fd, F_SETFL, fl | O_NONBLOCK);

        struct v4l2_format fmt = {0};
        fmt.type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
        fmt.fmt.pix.width = CAM_WIDTH;
        fmt.fmt.pix.height = CAM_HEIGHT;
        fmt.fmt.pix.pixelformat = V4L2_PIX_FMT_YUYV;
        fmt.fmt.pix.field = V4L2_FIELD_NONE;
        if (ioctl(cam_real_fd, VIDIOC_S_FMT, &fmt) != 0)
            plog("streamproxy: cam feeder: S_FMT failed: %s\n", strerror(errno));
        real_fsize = fmt.fmt.pix.sizeimage;
        if (fmt.fmt.pix.pixelformat != V4L2_PIX_FMT_YUYV)
            plog("streamproxy: cam feeder: camera negotiated fourcc 0x%08x, not YUYV\n",
                 fmt.fmt.pix.pixelformat);

        struct v4l2_requestbuffers rq = {0};
        rq.count = CAM_NUM_BUFS;
        rq.type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
        rq.memory = V4L2_MEMORY_MMAP;
        int ok = (ioctl(cam_real_fd, VIDIOC_REQBUFS, &rq) == 0);
        nbufs = ok ? rq.count : 0;
        for (uint32_t i = 0; ok && i < nbufs; i++) {
            struct v4l2_buffer b = {0};
            b.type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
            b.memory = V4L2_MEMORY_MMAP;
            b.index = i;
            if (ioctl(cam_real_fd, VIDIOC_QUERYBUF, &b) < 0) { ok=0; break; }
            bufs[i] = mmap(NULL, b.length, PROT_READ|PROT_WRITE,
                           MAP_SHARED, cam_real_fd, b.m.offset);
            if (bufs[i] == MAP_FAILED) { bufs[i]=NULL; ok=0; break; }
        }
        if (!ok) {
            plog("streamproxy: cam feeder: buffer setup failed\n");
            cam_real_fd = -1;  /* can't use it */
        } else {
            plog("streamproxy: cam feeder: camera buffers ready (%s)\n", cam_real_dev);
        }
    }

    while (cam_running) {
        int want = (access(trigger, F_OK) == 0);

        /* Start streaming */
        if (want && !streaming && cam_real_fd >= 0) {
            /* Queue all buffers */
            for (uint32_t i = 0; i < nbufs; i++) {
                struct v4l2_buffer b = {0};
                b.type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
                b.memory = V4L2_MEMORY_MMAP;
                b.index = i;
                ioctl(cam_real_fd, VIDIOC_QBUF, &b);
            }
            int t = V4L2_BUF_TYPE_VIDEO_CAPTURE;
            if (ioctl(cam_real_fd, VIDIOC_STREAMON, &t) == 0) {
                streaming = 1;
                plog("streamproxy: cam feeder: STREAMON (LED on)\n");
            } else {
                /* streaming stays 0 -> black-frame path sleeps 200ms, then
                 * the trigger poll retries STREAMON. No silent spin. */
                plog("streamproxy: cam feeder: STREAMON failed: %s (retry)\n",
                     strerror(errno));
            }
        }

        /* Stop streaming */
        if (!want && streaming) {
            int t = V4L2_BUF_TYPE_VIDEO_CAPTURE;
            ioctl(cam_real_fd, VIDIOC_STREAMOFF, &t);
            streaming = 0;
            plog("streamproxy: cam feeder: STREAMOFF (LED off)\n");
        }

        /* Write one frame */
        if (streaming) {
            struct v4l2_buffer b = {0};
            b.type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
            b.memory = V4L2_MEMORY_MMAP;
            if (ioctl(cam_real_fd, VIDIOC_DQBUF, &b) == 0) {
                ssize_t w = write(loop_fd, bufs[b.index], b.bytesused);
                ioctl(cam_real_fd, VIDIOC_QBUF, &b);
                static int fc = 0;
                if (++fc <= 3 || fc % 300 == 0)
                    plog("streamproxy: cam feeder: frame %d bytes=%u written=%zd\n",
                         fc, b.bytesused, w);
            } else {
                if (errno != EAGAIN) {
                    static int ec = 0;
                    if (++ec <= 5)
                        plog("streamproxy: cam feeder: DQBUF: %s\n", strerror(errno));
                }
                usleep(10000);
            }
        } else {
            /* Black frame at ~5fps */
            if (black) write(loop_fd, black, frame_size);
            usleep(200000);
        }
    }

    /* Cleanup */
    if (streaming) {
        int t = V4L2_BUF_TYPE_VIDEO_CAPTURE;
        ioctl(cam_real_fd, VIDIOC_STREAMOFF, &t);
    }
    for (uint32_t i = 0; i < nbufs; i++)
        if (bufs[i]) munmap(bufs[i], real_fsize);
    close(loop_fd);
    free(black);
    plog("streamproxy: cam feeder: exited\n");
    return NULL;
}

static void cam_feeder_start(void) {
    /* Detect the loopback FIRST — when no bridge is installed, bail out
     * BEFORE touching anything: machines where Wine's native V4L2 works
     * (e.g. Ubuntu 26.04) must keep their camera path completely
     * untouched. */
    cam_loopback_nr = cam_find_loopback();
    if (cam_loopback_nr < 0) return;

    /* Check sysfs — Wine needs device/modalias */
    char sysfs[256];
    snprintf(sysfs, sizeof(sysfs),
             "/sys/class/video4linux/video%d/device/modalias", cam_loopback_nr);
    if (access(sysfs, F_OK) != 0) {
        plog("streamproxy: cam feeder: loopback video%d found but sysfs not faked\n",
             cam_loopback_nr);
        return;
    }
    char dev[64];
    snprintf(dev, sizeof(dev), "/dev/video%d", cam_loopback_nr);
    if (access(dev, W_OK) != 0) return;

    /* Bridge ready — now reserve the real camera (before Wine scans).
     * The loopback is detected first so the scan can skip it: its faked
     * modalias also starts with "usb:" and would otherwise be picked as
     * the real camera when it has a lower video number.
     * open() alone does NOT turn on the LED — only STREAMON does.
     * This prevents Wine from accessing /dev/video0 (DQBUF race crash). */
    if (cam_find_real_device(cam_real_dev, sizeof(cam_real_dev)) == 0) {
        cam_real_fd = open(cam_real_dev, O_RDWR);
        if (cam_real_fd >= 0)
            plog("streamproxy: cam feeder: reserved %s (fd=%d, LED off)\n",
                 cam_real_dev, cam_real_fd);
    }

    plog("streamproxy: cam feeder: loopback /dev/video%d ready\n", cam_loopback_nr);
    cam_running = 1;
    pthread_create(&cam_thread, NULL, cam_feeder_thread, NULL);
}

__attribute__((constructor))
static void streamproxy_init(void) {
    cam_feeder_start();
}

__attribute__((destructor))
static void streamproxy_fini(void) {
    if (cam_running) {
        cam_running = 0;
        pthread_join(cam_thread, NULL);
    }
    if (cam_real_fd >= 0) close(cam_real_fd);
}

/* ==================================================================
 * SCREEN PROXY — unchanged from the reference build
 * ================================================================== */

static Display *src_dpy = NULL;
static xcb_connection_t *src_c = NULL;
static xcb_window_t src_root = 0;

static void signal_request(void) {
    const char *p = getenv("ZCALL_PROXY_REQUEST");
    if (!p) return;
    FILE *f = fopen(p, "w");
    if (f) fclose(f);
}

static int (*orig_io_handler)(Display *) = NULL;

static int src_io_handler(Display *d) {
    if (d == src_dpy) {
        plog("streamproxy: src connection broken, dropping cache\n");
        src_dpy = NULL;
        return 0;
    }
    if (orig_io_handler) return orig_io_handler(d);
    return 1;
}

static Display *ensure_src_dpy(void) {
    if (!src_dpy) {
        const char *n = getenv("ZCALL_PROXY_SRC");
        if (!n) n = ":99";
        src_dpy = XOpenDisplay(n);
        if (src_dpy) {
            orig_io_handler = XSetIOErrorHandler(src_io_handler);
            plog("streamproxy: libX11 src %s opened\n", n);
        } else {
            plog("streamproxy: cannot open src %s (not proxying)\n", n);
            signal_request();
        }
    }
    return src_dpy;
}

static xcb_connection_t *ensure_src_c(void) {
    if (!src_c) {
        const char *n = getenv("ZCALL_PROXY_SRC");
        if (!n) n = ":99";
        int scr = 0;
        src_c = xcb_connect(n, &scr);
        if (src_c && !xcb_connection_has_error(src_c)) {
            xcb_screen_iterator_t it =
                xcb_setup_roots_iterator(xcb_get_setup(src_c));
            if (it.rem) src_root = it.data->root;
            plog("streamproxy: xcb src %s opened\n", n);
        } else {
            if (src_c) xcb_disconnect(src_c);
            src_c = NULL;
            plog("streamproxy: cannot open xcb src %s\n", n);
            signal_request();
        }
    }
    return src_c;
}

/* ---- libX11 ---- */

typedef XImage *(*XGetImage_fn)(Display *, Drawable, int, int, unsigned int,
                                unsigned int, unsigned long, int);
typedef Bool (*XShmGetImage_fn)(Display *, Drawable, XImage *, int, int,
                                unsigned long);

static XGetImage_fn real_XGetImage = NULL;
static XShmGetImage_fn real_XShmGetImage = NULL;

static int is_root(Display *dpy, Drawable d) {
    return d == (Drawable)DefaultRootWindow(dpy);
}

XImage *XGetImage(Display *dpy, Drawable d, int x, int y, unsigned int w,
                  unsigned int h, unsigned long plane_mask, int format) {
    if (!real_XGetImage)
        real_XGetImage = (XGetImage_fn)dlsym(RTLD_NEXT, "XGetImage");
    if (ensure_src_dpy() && dpy != src_dpy && is_root(dpy, d)) {
        XImage *im = real_XGetImage(src_dpy, (Drawable)DefaultRootWindow(src_dpy),
                                    x, y, w, h, plane_mask, format);
        plog("streamproxy: XGetImage root %ux%u+%d+%d -> %s\n", w, h, x, y,
             im ? "proxied" : "src-failed, fell through");
        if (im) return im;
    }
    return real_XGetImage(dpy, d, x, y, w, h, plane_mask, format);
}

Bool XShmGetImage(Display *dpy, Drawable d, XImage *image, int x, int y,
                  unsigned long plane_mask) {
    if (!real_XShmGetImage)
        real_XShmGetImage = (XShmGetImage_fn)dlsym(RTLD_NEXT, "XShmGetImage");
    if (!real_XGetImage)
        real_XGetImage = (XGetImage_fn)dlsym(RTLD_NEXT, "XGetImage");
    if (ensure_src_dpy() && dpy != src_dpy && is_root(dpy, d) && image) {
        XImage *im = real_XGetImage(src_dpy, (Drawable)DefaultRootWindow(src_dpy),
                                    x, y, image->width, image->height,
                                    plane_mask, ZPixmap);
        if (im) {
            size_t copy = im->bytes_per_line < image->bytes_per_line
                              ? im->bytes_per_line
                              : image->bytes_per_line;
            for (unsigned int r = 0; r < (unsigned int)im->height; r++)
                memcpy(image->data + (size_t)r * image->bytes_per_line,
                       im->data + (size_t)r * im->bytes_per_line, copy);
            XDestroyImage(im);
            plog("streamproxy: XShmGetImage root %dx%d -> proxied\n",
                 image->width, image->height);
            return True;
        }
    }
    return real_XShmGetImage(dpy, d, image, x, y, plane_mask);
}

/* ---- xcb ---- */

typedef xcb_get_image_cookie_t (*xcb_get_image_fn)(xcb_connection_t *, uint8_t,
                                                   xcb_drawable_t, int16_t,
                                                   int16_t, uint16_t, uint16_t,
                                                   uint32_t);
typedef xcb_get_image_reply_t *(*xcb_get_image_reply_fn)(
    xcb_connection_t *, xcb_get_image_cookie_t, xcb_generic_error_t **);

static xcb_get_image_fn real_xcb_get_image = NULL;
static xcb_get_image_reply_fn real_xcb_get_image_reply = NULL;

#define PROXY_MAP_SIZE 64
static struct {
    uint64_t seq;
    int valid;
    int16_t x, y;
    uint16_t w, h;
} proxy_map[PROXY_MAP_SIZE];

static xcb_window_t conn_root(xcb_connection_t *c) {
    xcb_screen_iterator_t it = xcb_setup_roots_iterator(xcb_get_setup(c));
    return it.rem ? it.data->root : 0;
}

xcb_get_image_cookie_t xcb_get_image(xcb_connection_t *c, uint8_t format,
                                     xcb_drawable_t drawable, int16_t x,
                                     int16_t y, uint16_t width, uint16_t height,
                                     uint32_t plane_mask) {
    if (!real_xcb_get_image)
        real_xcb_get_image = (xcb_get_image_fn)dlsym(RTLD_NEXT, "xcb_get_image");
    xcb_get_image_cookie_t cookie =
        real_xcb_get_image(c, format, drawable, x, y, width, height, plane_mask);
    if (ensure_src_c() && c != src_c && drawable == conn_root(c)) {
        unsigned int slot = cookie.sequence % PROXY_MAP_SIZE;
        for (unsigned int i = 0; i < PROXY_MAP_SIZE; i++) {
            unsigned int s = (slot + i) % PROXY_MAP_SIZE;
            if (!proxy_map[s].valid) {
                proxy_map[s].valid = 1;
                proxy_map[s].seq = cookie.sequence;
                proxy_map[s].x = x;
                proxy_map[s].y = y;
                proxy_map[s].w = width;
                proxy_map[s].h = height;
                plog("streamproxy: xcb_get_image root %ux%u+%d+%d queued\n",
                     width, height, x, y);
                break;
            }
        }
    }
    return cookie;
}

xcb_get_image_reply_t *xcb_get_image_reply(xcb_connection_t *c,
                                           xcb_get_image_cookie_t cookie,
                                           xcb_generic_error_t **e) {
    if (!real_xcb_get_image_reply)
        real_xcb_get_image_reply =
            (xcb_get_image_reply_fn)dlsym(RTLD_NEXT, "xcb_get_image_reply");
    unsigned int slot = cookie.sequence % PROXY_MAP_SIZE;
    for (unsigned int i = 0; i < PROXY_MAP_SIZE; i++) {
        unsigned int s = (slot + i) % PROXY_MAP_SIZE;
        if (proxy_map[s].valid && proxy_map[s].seq == cookie.sequence) {
            proxy_map[s].valid = 0;
            xcb_generic_error_t *lerr = NULL;
            xcb_get_image_reply_t *rr = real_xcb_get_image_reply(c, cookie, &lerr);
            free(rr);
            xcb_get_image_cookie_t c2 =
                real_xcb_get_image(src_c, XCB_IMAGE_FORMAT_Z_PIXMAP, src_root,
                                   proxy_map[s].x, proxy_map[s].y,
                                   proxy_map[s].w, proxy_map[s].h, ~0);
            xcb_get_image_reply_t *r2 = real_xcb_get_image_reply(src_c, c2, NULL);
            if (r2) {
                uint8_t *data = (uint8_t *)(r2 + 1);
                size_t len = (size_t)r2->length * 4;
                xcb_get_image_reply_t *out =
                    malloc(sizeof(xcb_get_image_reply_t) + len);
                memset(out, 0, sizeof(xcb_get_image_reply_t));
                out->response_type = 1;
                out->depth = r2->depth;
                out->sequence = (uint16_t)cookie.sequence;
                out->visual = r2->visual;
                out->length = r2->length;
                memcpy(out + 1, data, len);
                free(r2);
                plog("streamproxy: xcb root grab %ux%u -> proxied\n",
                     proxy_map[s].w, proxy_map[s].h);
                return out;
            }
            plog("streamproxy: xcb src grab failed\n");
            return NULL;
        }
    }
    return real_xcb_get_image_reply(c, cookie, e);
}
