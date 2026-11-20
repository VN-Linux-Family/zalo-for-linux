#define _POSIX_C_SOURCE 200809L
#include <math.h>
#include <pulse/pulseaudio.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

typedef struct {
    pa_mainloop* ml;
    pa_context* ctx;
    int ready;
    int failed;
    char default_source[512];
    int got_server_info;
    uint32_t source_index;
    pa_cvolume source_vol;
    int got_source_info;
    int set_done;
    int set_ok;
} PulseVolOp;

static double now_sec(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec * 1e-9;
}

static void ctx_state_cb(pa_context* c, void* userdata) {
    PulseVolOp* op = (PulseVolOp*)userdata;
    switch (pa_context_get_state(c)) {
        case PA_CONTEXT_READY:
            op->ready = 1;
            break;
        case PA_CONTEXT_FAILED:
        case PA_CONTEXT_TERMINATED:
            op->failed = 1;
            break;
        default:
            break;
    }
}

static void server_info_cb(pa_context* c, const pa_server_info* i, void* userdata) {
    (void)c;
    PulseVolOp* op = (PulseVolOp*)userdata;
    if (i && i->default_source_name) {
        snprintf(op->default_source, sizeof(op->default_source), "%s", i->default_source_name);
    }
    op->got_server_info = 1;
}

static void source_info_cb(pa_context* c, const pa_source_info* i, int eol, void* userdata) {
    (void)c;
    PulseVolOp* op = (PulseVolOp*)userdata;
    if (eol < 0) {
        op->failed = 1;
        op->got_source_info = 1;
        return;
    }
    if (eol > 0) {
        op->got_source_info = 1;
        return;
    }
    if (i) {
        op->source_index = i->index;
        op->source_vol = i->volume;
        op->got_source_info = 1;
    }
}

static void set_vol_cb(pa_context* c, int success, void* userdata) {
    (void)c;
    PulseVolOp* op = (PulseVolOp*)userdata;
    op->set_ok = (success != 0);
    op->set_done = 1;
}

static int run_until(PulseVolOp* op, int* flag, double timeout_sec) {
    double deadline = now_sec() + timeout_sec;
    while (!(*flag) && !op->failed) {
        if (now_sec() >= deadline) {
            return -1;
        }
        if (pa_mainloop_prepare(op->ml, 100000) < 0) {
            return -1;
        }
        if (pa_mainloop_poll(op->ml) < 0) {
            return -1;
        }
        if (pa_mainloop_dispatch(op->ml) < 0) {
            return -1;
        }
    }
    return op->failed ? -1 : 0;
}

static int connect_pulse(PulseVolOp* op) {
    memset(op, 0, sizeof(*op));
    op->ml = pa_mainloop_new();
    if (!op->ml) {
        return -1;
    }
    pa_mainloop_api* api = pa_mainloop_get_api(op->ml);
    op->ctx = pa_context_new(api, "ZaloCall-VolCtl");
    if (!op->ctx) {
        pa_mainloop_free(op->ml);
        op->ml = NULL;
        return -1;
    }
    pa_context_set_state_callback(op->ctx, ctx_state_cb, op);
    if (pa_context_connect(op->ctx, NULL, PA_CONTEXT_NOAUTOSPAWN, NULL) < 0) {
        pa_context_unref(op->ctx);
        pa_mainloop_free(op->ml);
        op->ctx = NULL;
        op->ml = NULL;
        return -1;
    }
    if (run_until(op, &op->ready, 1.5) < 0) {
        pa_context_disconnect(op->ctx);
        pa_context_unref(op->ctx);
        pa_mainloop_free(op->ml);
        op->ctx = NULL;
        op->ml = NULL;
        return -1;
    }
    return 0;
}

static void disconnect_pulse(PulseVolOp* op) {
    if (op->ctx) {
        pa_context_disconnect(op->ctx);
        pa_context_unref(op->ctx);
        op->ctx = NULL;
    }
    if (op->ml) {
        pa_mainloop_free(op->ml);
        op->ml = NULL;
    }
}

static const char* resolve_source(PulseVolOp* op, const char* source_name) {
    if (source_name && source_name[0] != '\0' && strcmp(source_name, "@DEFAULT_SOURCE@") != 0) {
        return source_name;
    }
    pa_operation* o = pa_context_get_server_info(op->ctx, server_info_cb, op);
    if (!o) {
        return NULL;
    }
    int rc = run_until(op, &op->got_server_info, 1.5);
    pa_operation_unref(o);
    if (rc < 0 || op->default_source[0] == '\0') {
        return NULL;
    }
    return op->default_source;
}

static int fetch_source_info(PulseVolOp* op, const char* resolved_name) {
    op->got_source_info = 0;
    pa_operation* o = pa_context_get_source_info_by_name(op->ctx, resolved_name, source_info_cb, op);
    if (!o) {
        return -1;
    }
    int rc = run_until(op, &op->got_source_info, 1.5);
    pa_operation_unref(o);
    if (rc < 0 || op->source_vol.channels == 0) {
        return -1;
    }
    return 0;
}

int zcall_pulse_get_source_volume(const char* source_name, int* out_pct) {
    if (!out_pct) {
        return -1;
    }
    PulseVolOp op;
    if (connect_pulse(&op) < 0) {
        return -1;
    }
    const char* resolved = resolve_source(&op, source_name);
    if (!resolved || fetch_source_info(&op, resolved) < 0) {
        disconnect_pulse(&op);
        return -1;
    }
    pa_volume_t avg = pa_cvolume_avg(&op.source_vol);
    long pct = lround((double)avg * 100.0 / (double)PA_VOLUME_NORM);
    if (pct < 0) pct = 0;
    if (pct > 150) pct = 150;
    *out_pct = (int)pct;
    disconnect_pulse(&op);
    return 0;
}

int zcall_pulse_set_source_volume(const char* source_name, int pct) {
    if (pct < 0) pct = 0;
    if (pct > 150) pct = 150;
    PulseVolOp op;
    if (connect_pulse(&op) < 0) {
        return -1;
    }
    const char* resolved = resolve_source(&op, source_name);
    if (!resolved || fetch_source_info(&op, resolved) < 0) {
        disconnect_pulse(&op);
        return -1;
    }
    uint8_t channels = op.source_vol.channels;
    if (channels == 0) {
        channels = 1;
    }
    pa_volume_t raw = (pa_volume_t)llround((double)PA_VOLUME_NORM * ((double)pct / 100.0));
    pa_cvolume cv;
    pa_cvolume_set(&cv, channels, raw);
    pa_operation* o = pa_context_set_source_volume_by_index(op.ctx, op.source_index, &cv, set_vol_cb, &op);
    if (!o) {
        disconnect_pulse(&op);
        return -1;
    }
    int rc = run_until(&op, &op.set_done, 1.5);
    pa_operation_unref(o);
    int ok = (rc == 0 && op.set_ok);
    disconnect_pulse(&op);
    return ok ? 0 : -1;
}
