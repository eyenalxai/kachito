// Measure libmpv software-render cost for a local file.
// build: cc -O2 bench.c -o bench $(pkg-config --cflags --libs mpv)
#include <mpv/client.h>
#include <mpv/render.h>
#include <stdint.h>
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

int main(int argc, char **argv) {
    if (argc < 5) {
        fprintf(stderr, "usage: %s FILE WIDTH HEIGHT SECONDS [hwdec]\n", argv[0]);
        return 2;
    }
    const char *path = argv[1];
    int w = atoi(argv[2]);
    int h = atoi(argv[3]);
    double seconds = atof(argv[4]);
    const char *hwdec = argc > 5 ? argv[5] : "no";

    mpv_handle *mpv = mpv_create();
    if (!mpv) { fprintf(stderr, "mpv_create failed\n"); return 1; }
    check(mpv_set_option_string(mpv, "vo", "libmpv"), "vo");
    check(mpv_set_option_string(mpv, "ao", "null"), "ao");
    check(mpv_set_option_string(mpv, "idle", "yes"), "idle");
    check(mpv_set_option_string(mpv, "keep-open", "no"), "keep-open");
    check(mpv_set_option_string(mpv, "hwdec", hwdec), "hwdec");
    check(mpv_set_option_string(mpv, "video-sync", "audio"), "video-sync");
    check(mpv_set_option_string(mpv, "video-timing-offset", "0"), "video-timing-offset");
    check(mpv_set_option_string(mpv, "osd-level", "0"), "osd-level");
    check(mpv_set_option_string(mpv, "sub-visibility", argc > 6 ? "yes" : "no"), "sub-visibility");
    check(mpv_set_option_string(mpv, "osc", "no"), "osc");
    check(mpv_set_option_string(mpv, "input-default-bindings", "no"), "input-default-bindings");
    check(mpv_initialize(mpv), "mpv_initialize");

    mpv_render_param params[] = {
        {MPV_RENDER_PARAM_API_TYPE, (void *)MPV_RENDER_API_TYPE_SW},
        {MPV_RENDER_PARAM_INVALID, NULL},
    };
    mpv_render_context *ctx = NULL;
    check(mpv_render_context_create(&ctx, mpv, params), "render create");
    if (!ctx) { fprintf(stderr, "no render context\n"); return 1; }

    size_t stride = (size_t)w * 4;
    unsigned char *buf = malloc(stride * (size_t)h);
    if (!buf) { fprintf(stderr, "oom\n"); return 1; }

    const char *load[] = {"loadfile", path, NULL};
    check(mpv_command(mpv, load), "loadfile");

    int loaded = 0;
    double start = 0.0;
    uint64_t frames = 0;
    double total = 0.0, worst = 0.0;
    int running = 1;
    while (running) {
        mpv_event *ev = mpv_wait_event(mpv, 0.0);
        if (ev && ev->event_id == MPV_EVENT_FILE_LOADED) {
            loaded = 1;
            start = now_ms();
            if (argc > 6) { const char *subadd[] = {"sub-add", argv[6], "select", NULL}; mpv_command(mpv, subadd); }
        } else if (ev && ev->event_id == MPV_EVENT_END_FILE) {
            running = 0;
        }

        uint64_t flags = mpv_render_context_update(ctx);
        if (flags & MPV_RENDER_UPDATE_FRAME) {
            int size[2] = {w, h};
            mpv_render_param rparams[] = {
                {MPV_RENDER_PARAM_SW_SIZE, &size},
                {MPV_RENDER_PARAM_SW_FORMAT, "bgr0"},
                {MPV_RENDER_PARAM_SW_STRIDE, &stride},
                {MPV_RENDER_PARAM_SW_POINTER, buf},
                {MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME, &(int){0}},
                {MPV_RENDER_PARAM_INVALID, NULL},
            };
            double t0 = now_ms();
            int rc = mpv_render_context_render(ctx, rparams);
            double dt = now_ms() - t0;
            if (rc < 0) { fprintf(stderr, "render: %s\n", mpv_error_string(rc)); break; }
            mpv_render_context_report_swap(ctx);
            frames++;
            total += dt;
            if (dt > worst) worst = dt;
            if (loaded && now_ms() - start >= seconds * 1000.0) running = 0;
        } else {
            struct timespec ts = {0, 1000000};
            nanosleep(&ts, NULL);
            if (loaded && now_ms() - start >= seconds * 1000.0) running = 0;
        }
    }

    printf("%s %dx%d hwdec=%s frames=%llu avg=%.2fms worst=%.2fms fps=%.1f\n",
           path, w, h, hwdec, (unsigned long long)frames,
           frames ? total / frames : 0.0, worst,
           frames ? frames * 1000.0 / total : 0.0);
    free(buf);
    mpv_render_context_free(ctx);
    mpv_terminate_destroy(mpv);
    return 0;
}
