#define _GNU_SOURCE
#include <stdint.h>
#include <stddef.h>

typedef struct zcall_pw_capture zcall_pw_capture;
typedef void (*zcall_pw_frame_fn)(void *, const uint8_t *, uint32_t,
                                   uint32_t, uint32_t, int32_t, uint32_t, uint32_t);

void zcall_pw_capture_free(zcall_pw_capture *capture);

#include <errno.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <pthread.h>

#include <pipewire/pipewire.h>
#include <pipewire/main-loop.h>
#include <pipewire/context.h>
#include <pipewire/core.h>
#include <pipewire/proxy.h>
#include <pipewire/stream.h>
#include <spa/buffer/buffer.h>
#include <spa/pod/builder.h>
#include <spa/pod/vararg.h>
#include <spa/param/format.h>
#include <spa/param/video/format-utils.h>
#include <spa/param/video/raw-utils.h>
#include <spa/utils/defs.h>

static pthread_once_t pipewire_init_once = PTHREAD_ONCE_INIT;

static void initialize_pipewire(void)
{
    pw_init(NULL, NULL);
}

struct zcall_pw_capture {
    struct pw_main_loop *main_loop;
    struct pw_context *context;
    struct pw_core *core;
    struct pw_registry *registry;
    struct pw_stream *stream;
    struct spa_hook core_listener;
    struct spa_hook registry_listener;
    struct spa_hook stream_listener;
    zcall_pw_frame_fn on_frame;
    void *userdata;
    uint32_t node_id;
    uint32_t width;
    uint32_t height;
    int node_seen;
    int node_seen_once;
    int streaming;
    int stream_started;
    int stream_connected;
    int disconnecting;
    char error[512];
};

static void set_error(struct zcall_pw_capture *capture, const char *message)
{
    if (capture == NULL || capture->error[0] != '\0')
        return;
    snprintf(capture->error, sizeof(capture->error), "%s", message ? message : "unknown PipeWire error");
}

static void on_core_error(void *userdata, uint32_t id, int seq, int result, const char *message)
{
    struct zcall_pw_capture *capture = userdata;
    char detail[512];
    snprintf(detail, sizeof(detail),
             "PipeWire core error: object=%u, seq=%d, result=%d, message=%s",
             id, seq, result, message ? message : "(no message)");
    set_error(capture, detail);
}

static const struct pw_core_events core_events = {
    PW_VERSION_CORE_EVENTS,
    .error = on_core_error,
};

static void on_global(void *userdata, uint32_t id, uint32_t permissions,
                      const char *type, uint32_t version, const struct spa_dict *props)
{
    struct zcall_pw_capture *capture = userdata;
    (void)permissions;
    (void)type;
    (void)version;
    (void)props;
    if (id == capture->node_id) {
        capture->node_seen = 1;
        capture->node_seen_once = 1;
        fprintf(stderr, "[zcall-screen] PipeWire target appeared: node_id=%u\n", id);
    }
}

static void on_global_remove(void *userdata, uint32_t id)
{
    struct zcall_pw_capture *capture = userdata;
    if (id == capture->node_id) {
        capture->node_seen = 0;
        fprintf(stderr, "[zcall-screen] PipeWire target disappeared: node_id=%u\n", id);
        if (capture->node_seen_once && !capture->disconnecting)
            set_error(capture, "PipeWire screen source disappeared");
    }
}

static const struct pw_registry_events registry_events = {
    PW_VERSION_REGISTRY_EVENTS,
    .global = on_global,
    .global_remove = on_global_remove,
};

static void on_state_changed(void *userdata, enum pw_stream_state old_state,
                              enum pw_stream_state state, const char *error)
{
    struct zcall_pw_capture *capture = userdata;
    fprintf(stderr, "[zcall-screen] PipeWire stream state: %s -> %s%s%s\n",
            pw_stream_state_as_string(old_state), pw_stream_state_as_string(state),
            error != NULL && error[0] != '\0' ? ": " : "",
            error != NULL ? error : "");
    capture->streaming = state == PW_STREAM_STATE_STREAMING;
    if (capture->streaming)
        capture->stream_started = 1;
    if (capture->disconnecting)
        return;
    if (state == PW_STREAM_STATE_ERROR) {
        char detail[512];
        snprintf(detail, sizeof(detail), "PipeWire screen stream failed: %s",
                 error != NULL ? error : "unknown error");
        set_error(capture, detail);
    } else if (state == PW_STREAM_STATE_UNCONNECTED && capture->stream_started) {
        set_error(capture, "PipeWire screen stream disconnected");
    }
}

static void on_param_changed(void *userdata, uint32_t id, const struct spa_pod *param)
{
    struct zcall_pw_capture *capture = userdata;
    struct spa_video_info info = {0};
    if (id != SPA_PARAM_Format || param == NULL)
        return;
    if (spa_format_video_parse(param, &info) < 0 ||
        info.media_type != SPA_MEDIA_TYPE_video ||
        info.media_subtype != SPA_MEDIA_SUBTYPE_raw)
        return;
    capture->width = info.info.raw.size.width;
    capture->height = info.info.raw.size.height;
}

static void on_process(void *userdata)
{
    struct zcall_pw_capture *capture = userdata;
    struct pw_buffer *pw_buffer = pw_stream_dequeue_buffer(capture->stream);
    struct spa_buffer *buffer;
    struct spa_data *data;
    struct spa_chunk *chunk;

    if (pw_buffer == NULL)
        return;
    buffer = pw_buffer->buffer;
    if (buffer != NULL && buffer->n_datas > 0) {
        data = &buffer->datas[0];
        chunk = data->chunk;
        if (data->data != NULL && chunk != NULL && capture->width > 0 && capture->height > 0 &&
            capture->on_frame != NULL) {
            capture->on_frame(capture->userdata, data->data, data->maxsize,
                              chunk->offset, chunk->size, chunk->stride,
                              capture->width, capture->height);
        }
    }
    if (pw_stream_queue_buffer(capture->stream, pw_buffer) < 0)
        set_error(capture, "Return PipeWire capture buffer failed");
}

static const struct pw_stream_events stream_events = {
    PW_VERSION_STREAM_EVENTS,
    .state_changed = on_state_changed,
    .param_changed = on_param_changed,
    .process = on_process,
};

zcall_pw_capture *zcall_pw_capture_new(int remote_fd, uint32_t node_id,
                                       zcall_pw_frame_fn on_frame, void *userdata,
                                       char *error, size_t error_size)
{
    struct zcall_pw_capture *capture = calloc(1, sizeof(*capture));
    if (capture == NULL) {
        snprintf(error, error_size, "Allocate PipeWire capture state: %s", strerror(errno));
        close(remote_fd);
        return NULL;
    }
    capture->node_id = node_id;
    capture->userdata = userdata;
    capture->on_frame = on_frame;

    pthread_once(&pipewire_init_once, initialize_pipewire);
    capture->main_loop = pw_main_loop_new(NULL);
    if (capture->main_loop == NULL) {
        snprintf(error, error_size, "Create PipeWire main loop: %s", strerror(errno));
        goto fail_fd;
    }
    capture->context = pw_context_new(pw_main_loop_get_loop(capture->main_loop), NULL, 0);
    if (capture->context == NULL) {
        snprintf(error, error_size, "Create PipeWire context: %s", strerror(errno));
        goto fail_fd;
    }
    capture->core = pw_context_connect_fd(capture->context, remote_fd, NULL, 0);
    if (capture->core == NULL) {
        snprintf(error, error_size, "Connect to portal PipeWire stream: %s", strerror(errno));
        remote_fd = -1;
        goto fail;
    }
    remote_fd = -1;
    pw_core_add_listener(capture->core, &capture->core_listener, &core_events, capture);
    capture->registry = pw_core_get_registry(capture->core, PW_VERSION_REGISTRY, 0);
    if (capture->registry == NULL) {
        snprintf(error, error_size, "Get portal PipeWire registry: %s", strerror(errno));
        goto fail;
    }
    pw_registry_add_listener(capture->registry, &capture->registry_listener,
                             &registry_events, capture);
    return capture;

fail_fd:
    if (remote_fd >= 0)
        close(remote_fd);
fail:
    zcall_pw_capture_free(capture);
    return NULL;
}

int zcall_pw_capture_node_seen(const zcall_pw_capture *capture)
{
    return capture != NULL && capture->node_seen;
}

int zcall_pw_capture_streaming(const zcall_pw_capture *capture)
{
    return capture != NULL && capture->streaming;
}

int zcall_pw_capture_connect(zcall_pw_capture *capture)
{
    uint8_t pod_buffer[1024];
    struct spa_pod_builder builder = SPA_POD_BUILDER_INIT(pod_buffer, sizeof(pod_buffer));
    const struct spa_pod *params[1];
    struct spa_rectangle default_size = SPA_RECTANGLE(1920, 1080);
    struct spa_rectangle minimum_size = SPA_RECTANGLE(320, 240);
    struct spa_rectangle maximum_size = SPA_RECTANGLE(1920, 1080);
    struct pw_properties *properties;
    int result;

    if (capture == NULL || capture->core == NULL)
        return -EINVAL;
    properties = pw_properties_new(
        PW_KEY_MEDIA_TYPE, "Video",
        PW_KEY_MEDIA_CATEGORY, "Capture",
        PW_KEY_MEDIA_ROLE, "Screen",
        NULL);
    if (properties == NULL) {
        set_error(capture, "Create PipeWire screen stream properties failed");
        return -ENOMEM;
    }
    capture->stream = pw_stream_new(capture->core, "Zalo Call Screen Share", properties);
    if (capture->stream == NULL) {
        set_error(capture, "Create PipeWire screen stream failed");
        return -errno;
    }
    pw_stream_add_listener(capture->stream, &capture->stream_listener, &stream_events, capture);

    params[0] = spa_pod_builder_add_object(
        &builder,
        SPA_TYPE_OBJECT_Format, SPA_PARAM_EnumFormat,
        SPA_FORMAT_mediaType, SPA_POD_Id(SPA_MEDIA_TYPE_video),
        SPA_FORMAT_mediaSubtype, SPA_POD_Id(SPA_MEDIA_SUBTYPE_raw),
        SPA_FORMAT_VIDEO_format,
            SPA_POD_CHOICE_ENUM_Id(3, SPA_VIDEO_FORMAT_BGRA,
                                   SPA_VIDEO_FORMAT_BGRA, SPA_VIDEO_FORMAT_BGRx),
        SPA_FORMAT_VIDEO_size,
            "?rR", 3, &default_size, &minimum_size, &maximum_size);
    if (params[0] == NULL) {
        set_error(capture, "Serialize PipeWire video format failed");
        return -EINVAL;
    }

    result = pw_stream_connect(capture->stream, PW_DIRECTION_INPUT, capture->node_id,
                               PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS,
                               params, 1);
    if (result < 0) {
        char detail[512];
        snprintf(detail, sizeof(detail), "Negotiate PipeWire screen node %u (BGRA/BGRx): %s",
                 capture->node_id, strerror(-result));
        set_error(capture, detail);
        return result;
    }
    capture->stream_connected = 1;
    return 0;
}

int zcall_pw_capture_iterate(zcall_pw_capture *capture, int timeout_ms)
{
    if (capture == NULL || capture->main_loop == NULL)
        return -EINVAL;
    return pw_loop_iterate(pw_main_loop_get_loop(capture->main_loop), timeout_ms);
}

void zcall_pw_capture_error(const zcall_pw_capture *capture, char *output, size_t output_size)
{
    if (output == NULL || output_size == 0)
        return;
    snprintf(output, output_size, "%s", capture == NULL ? "" : capture->error);
}

static void disconnect_stream(zcall_pw_capture *capture)
{
    if (capture->stream != NULL && capture->stream_connected) {
        capture->disconnecting = 1;
        (void)pw_stream_disconnect(capture->stream);
        capture->stream_connected = 0;
    }
}

void zcall_pw_capture_disconnect(zcall_pw_capture *capture)
{
    if (capture == NULL || capture->stream == NULL)
        return;
    disconnect_stream(capture);
    if (capture->main_loop != NULL)
        (void)pw_loop_iterate(pw_main_loop_get_loop(capture->main_loop), 50);
}

void zcall_pw_capture_free(zcall_pw_capture *capture)
{
    if (capture == NULL)
        return;
    if (capture->stream != NULL) {
        disconnect_stream(capture);
        spa_hook_remove(&capture->stream_listener);
        pw_stream_destroy(capture->stream);
    }
    if (capture->registry != NULL) {
        spa_hook_remove(&capture->registry_listener);
        pw_proxy_destroy((struct pw_proxy *)capture->registry);
    }
    if (capture->core != NULL) {
        spa_hook_remove(&capture->core_listener);
        pw_core_disconnect(capture->core);
    }
    if (capture->context != NULL)
        pw_context_destroy(capture->context);
    if (capture->main_loop != NULL)
        pw_main_loop_destroy(capture->main_loop);
    free(capture);
}
