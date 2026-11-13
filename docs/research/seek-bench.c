// Time a libmpv seek to MPV_EVENT_PLAYBACK_RESTART.
// This is the benchmark used for docs/research/instant-seeking.md.
//
// build:
//   cc -O2 seek-bench.c -o seek-bench $(pkg-config --cflags --libs mpv)
//
// usage:
//   ./seek-bench FILE TARGET MODE HWDEC [PAUSE|PLAY]
//
// example:
//   ./seek-bench video.mp4 7.0 absolute+exact auto-safe PAUSE
//
// MODE is an mpv seek flag list, e.g. absolute+exact or absolute+keyframes.
// The options mirror src/engine.rs: vo=libmpv, software render, video-sync=audio,
// video-timing-offset=0, and the same hwdec choice. The render target is
// 1280x720 bgr0, the same size the app uses.
#include <mpv/client.h>
#include <mpv/render.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static double now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000.0 + ts.tv_nsec / 1e6;
}

static void check(int rc, const char *what) {
    if (rc < 0) {
        fprintf(stderr, "%s: %s\n", what, mpv_error_string(rc));
        exit(1);
    }
}

static double prop_double(mpv_handle *mpv, const char *name) {
    double value = -1.0;
    if (mpv_get_property(mpv, name, MPV_FORMAT_DOUBLE, &value) < 0) {
        return -1.0;
    }
    return value;
}

static void render(mpv_render_context *ctx, unsigned char *buffer, int width, int height) {
    size_t stride = (size_t)width * 4;
    int size[2] = {width, height};
    mpv_render_param params[] = {
        {MPV_RENDER_PARAM_SW_SIZE, &size},
        {MPV_RENDER_PARAM_SW_FORMAT, "bgr0"},
        {MPV_RENDER_PARAM_SW_STRIDE, &stride},
        {MPV_RENDER_PARAM_SW_POINTER, buffer},
        {MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME, &(int){0}},
        {MPV_RENDER_PARAM_INVALID, NULL},
    };
    mpv_render_context_render(ctx, params);
    mpv_render_context_report_swap(ctx);
}

int main(int argc, char **argv) {
    if (argc < 6) {
        fprintf(stderr, "usage: %s FILE TARGET MODE HWDEC [PAUSE|PLAY]\n", argv[0]);
        return 2;
    }
    const char *path = argv[1];
    double target = atof(argv[2]);
    const char *mode = argv[3];
    const char *hwdec = argv[4];
    int paused = strcmp(argv[5], "PLAY") != 0;

    mpv_handle *mpv = mpv_create();
    if (!mpv) {
        fprintf(stderr, "mpv_create failed\n");
        return 1;
    }
    check(mpv_set_option_string(mpv, "vo", "libmpv"), "vo");
    check(mpv_set_option_string(mpv, "ao", "null"), "ao");
    check(mpv_set_option_string(mpv, "idle", "yes"), "idle");
    check(mpv_set_option_string(mpv, "keep-open", "yes"), "keep-open");
    check(mpv_set_option_string(mpv, "pause", paused ? "yes" : "no"), "pause");
    check(mpv_set_option_string(mpv, "hwdec", hwdec), "hwdec");
    check(mpv_set_option_string(mpv, "video-sync", "audio"), "video-sync");
    check(mpv_set_option_string(mpv, "video-timing-offset", "0"), "video-timing-offset");
    check(mpv_set_option_string(mpv, "osd-level", "0"), "osd-level");
    check(mpv_set_option_string(mpv, "osc", "no"), "osc");
    check(mpv_set_option_string(mpv, "input-default-bindings", "no"), "input-default-bindings");
    check(mpv_initialize(mpv), "mpv_initialize");

    mpv_render_param render_params[] = {
        {MPV_RENDER_PARAM_API_TYPE, (void *)MPV_RENDER_API_TYPE_SW},
        {MPV_RENDER_PARAM_INVALID, NULL},
    };
    mpv_render_context *ctx = NULL;
    check(mpv_render_context_create(&ctx, mpv, render_params), "render context");

    int width = 1280;
    int height = 720;
    unsigned char *buffer = malloc((size_t)width * 4 * (size_t)height);
    if (!buffer) {
        fprintf(stderr, "out of memory\n");
        return 1;
    }

    const char *load[] = {"loadfile", path, NULL};
    check(mpv_command(mpv, load), "loadfile");

    int loaded = 0;
    while (!loaded) {
        mpv_event *event = mpv_wait_event(mpv, 0.01);
        if (event->event_id == MPV_EVENT_FILE_LOADED) {
            loaded = 1;
        } else if (event->event_id == MPV_EVENT_END_FILE) {
            fprintf(stderr, "load failed\n");
            return 1;
        }
    }

    // Let the initial frame and the pause transition settle, and drain frames.
    double settle = now_ms();
    while (now_ms() - settle < 400) {
        mpv_wait_event(mpv, 0.0);
        uint64_t flags = mpv_render_context_update(ctx);
        if (flags & MPV_RENDER_UPDATE_FRAME) {
            render(ctx, buffer, width, height);
        } else {
            struct timespec ts = {0, 1000000};
            nanosleep(&ts, NULL);
        }
    }
    while (mpv_wait_event(mpv, 0.0)->event_id != MPV_EVENT_NONE) {
    }
    for (int i = 0; i < 200; i++) {
        uint64_t flags = mpv_render_context_update(ctx);
        if (!(flags & MPV_RENDER_UPDATE_FRAME)) {
            break;
        }
        render(ctx, buffer, width, height);
    }

    char time_text[64];
    snprintf(time_text, sizeof time_text, "%.3f", target);
    const char *seek[] = {"seek", time_text, mode, NULL};
    double start = now_ms();
    check(mpv_command(mpv, seek), "seek");

    int saw_seek = 0;
    double restart_ms = -1.0;
    double deadline = start + 30000.0;
    while (now_ms() < deadline) {
        mpv_event *event = mpv_wait_event(mpv, 0.001);
        if (event->event_id == MPV_EVENT_SEEK) {
            saw_seek = 1;
        }
        if (saw_seek && event->event_id == MPV_EVENT_PLAYBACK_RESTART) {
            restart_ms = now_ms() - start;
            break;
        }
        uint64_t flags = mpv_render_context_update(ctx);
        if (flags & MPV_RENDER_UPDATE_FRAME) {
            render(ctx, buffer, width, height);
        }
    }

    printf("target=%.3f mode=%s hwdec=%s %s restart=%.1fms time-pos=%.3f\n",
           target, mode, hwdec, paused ? "PAUSE" : "PLAY",
           restart_ms, prop_double(mpv, "time-pos"));

    free(buffer);
    mpv_render_context_free(ctx);
    mpv_terminate_destroy(mpv);
    return 0;
}
