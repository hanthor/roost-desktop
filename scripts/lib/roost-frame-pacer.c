/* CI-only native client. Presentation cadence is not input-to-frame latency. */
#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <inttypes.h>
#include <poll.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>
#include <wayland-client.h>
#include "xdg-shell-client-protocol.h"
#include "presentation-time-client-protocol.h"
#include "idle-inhibit-client-protocol.h"

#define WIDTH 320
#define HEIGHT 180
#define BYTES (WIDTH * HEIGHT * 4)
#define TARGET 240
#define MAX_COMMITS 1200
struct sample { uint64_t commit, timestamp, sequence; uint32_t refresh, flags; };
struct state;
struct buffer { struct wl_buffer *proxy; uint32_t *pixels; bool busy; };
struct feedback { struct state *state; struct wp_presentation_feedback *proxy; uint64_t commit; struct feedback *next; };
struct state {
    struct wl_display *display;
    struct wl_compositor *compositor;
    struct wl_shm *shm;
    struct xdg_wm_base *wm;
    struct wp_presentation *presentation;
    struct zwp_idle_inhibit_manager_v1 *idle_manager;
    struct zwp_idle_inhibitor_v1 *inhibitor;
    struct wl_surface *surface;
    struct xdg_surface *xdg;
    struct xdg_toplevel *toplevel;
    struct wl_callback *callback;
    struct buffer buffers[2];
    struct feedback *feedbacks;
    struct sample samples[TARGET];
    uint32_t clock_id;
    bool clock_known, configured, frame_ready, closed;
    unsigned presented, discarded, commits;
    unsigned total_presented, batch, batch_commit_start;
    bool endurance;
    double started, last_presented;
};
static void print_samples(struct state *s);
static double monotonic_s(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) { perror("clock_gettime"); exit(1); }
    return ts.tv_sec + ts.tv_nsec / 1e9;
}
static void buffer_release(void *data, struct wl_buffer *proxy) { (void)proxy; ((struct buffer *)data)->busy = false; }
static const struct wl_buffer_listener buffer_listener = { .release = buffer_release };
static void frame_done(void *data, struct wl_callback *proxy, uint32_t time) {
    (void)time;
    struct state *s = data;
    wl_callback_destroy(proxy); s->callback = NULL; s->frame_ready = true;
}
static const struct wl_callback_listener frame_listener = { .done = frame_done };
static void feedback_remove(struct feedback *f) {
    struct feedback **link = &f->state->feedbacks;
    while (*link && *link != f) link = &(*link)->next;
    if (*link) *link = f->next;
    wp_presentation_feedback_destroy(f->proxy); free(f);
}
static void sync_output(void *data, struct wp_presentation_feedback *proxy, struct wl_output *output) {
    (void)data; (void)proxy; (void)output;
}
static void presented(void *data, struct wp_presentation_feedback *proxy,
                      uint32_t sec_hi, uint32_t sec_lo, uint32_t ns, uint32_t refresh,
                      uint32_t seq_hi, uint32_t seq_lo, uint32_t flags) {
    (void)proxy;
    struct feedback *f = data;
    struct state *s = f->state;
    if (ns >= 1000000000) { fprintf(stderr, "invalid presentation nanoseconds\n"); s->closed = true; }
    else if (s->presented < TARGET) {
        s->samples[s->presented++] = (struct sample){f->commit,
            (((uint64_t)sec_hi << 32) | sec_lo) * UINT64_C(1000000000) + ns,
            ((uint64_t)seq_hi << 32) | seq_lo, refresh, flags};
        s->total_presented++;
        s->last_presented = monotonic_s();
        if (s->endurance && s->presented == TARGET) {
            print_samples(s);
            s->presented = 0; s->batch++; s->batch_commit_start = s->commits;
        }
    }
    feedback_remove(f);
}
static void discarded(void *data, struct wp_presentation_feedback *proxy) {
    (void)proxy;
    struct feedback *f = data; f->state->discarded++; feedback_remove(f);
}
static const struct wp_presentation_feedback_listener feedback_listener = {
    .sync_output = sync_output, .presented = presented, .discarded = discarded
};
static void clock_id(void *data, struct wp_presentation *proxy, uint32_t id) {
    (void)proxy; struct state *s = data; s->clock_id = id; s->clock_known = true;
}
static const struct wp_presentation_listener presentation_listener = { .clock_id = clock_id };
static void ping(void *data, struct xdg_wm_base *proxy, uint32_t serial) { (void)data; xdg_wm_base_pong(proxy, serial); }
static const struct xdg_wm_base_listener wm_listener = { .ping = ping };
static void configure(void *data, struct xdg_surface *proxy, uint32_t serial) {
    xdg_surface_ack_configure(proxy, serial); ((struct state *)data)->configured = true;
}
static const struct xdg_surface_listener xdg_listener = { .configure = configure };
static void top_configure(void *data, struct xdg_toplevel *proxy, int32_t width, int32_t height, struct wl_array *states) {
    (void)data; (void)proxy; (void)width; (void)height; (void)states;
}
static void close_window(void *data, struct xdg_toplevel *proxy) { (void)proxy; ((struct state *)data)->closed = true; }
static const struct xdg_toplevel_listener top_listener = { .configure = top_configure, .close = close_window };
static void global(void *data, struct wl_registry *registry, uint32_t name, const char *interface, uint32_t version) {
    struct state *s = data;
    if (!strcmp(interface, "wl_compositor")) s->compositor = wl_registry_bind(registry, name, &wl_compositor_interface, version < 4 ? version : 4);
    else if (!strcmp(interface, "wl_shm")) s->shm = wl_registry_bind(registry, name, &wl_shm_interface, 1);
    else if (!strcmp(interface, "xdg_wm_base")) {
        s->wm = wl_registry_bind(registry, name, &xdg_wm_base_interface, 1);
        xdg_wm_base_add_listener(s->wm, &wm_listener, s);
    } else if (!strcmp(interface, "wp_presentation")) {
        s->presentation = wl_registry_bind(registry, name, &wp_presentation_interface, 1);
        wp_presentation_add_listener(s->presentation, &presentation_listener, s);
    } else if (s->endurance && !strcmp(interface, "zwp_idle_inhibit_manager_v1")) {
        s->idle_manager = wl_registry_bind(registry, name, &zwp_idle_inhibit_manager_v1_interface, 1);
    }
}
static void global_remove(void *data, struct wl_registry *registry, uint32_t name) { (void)data; (void)registry; (void)name; }
static const struct wl_registry_listener registry_listener = { .global = global, .global_remove = global_remove };
static int make_buffers(struct state *s) {
    char path[] = "/tmp/roost-frame-pacer-XXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) return -1;
    unlink(path);
    if (ftruncate(fd, BYTES * 2) != 0) { close(fd); return -1; }
    void *pixels = mmap(NULL, BYTES * 2, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (pixels == MAP_FAILED) { close(fd); return -1; }
    struct wl_shm_pool *pool = wl_shm_create_pool(s->shm, fd, BYTES * 2);
    close(fd);
    for (int i = 0; i < 2; i++) {
        s->buffers[i].pixels = (uint32_t *)pixels + WIDTH * HEIGHT * i;
        s->buffers[i].proxy = wl_shm_pool_create_buffer(pool, BYTES * i, WIDTH, HEIGHT, WIDTH * 4, WL_SHM_FORMAT_XRGB8888);
        wl_buffer_add_listener(s->buffers[i].proxy, &buffer_listener, &s->buffers[i]);
    }
    wl_shm_pool_destroy(pool);
    return 0;
}
static void draw(struct state *s) {
    if (!s->configured || !s->frame_ready || s->commits - s->batch_commit_start >= MAX_COMMITS) return;
    struct buffer *b = !s->buffers[0].busy ? &s->buffers[0] : (!s->buffers[1].busy ? &s->buffers[1] : NULL);
    if (!b) return;
    for (int y = 0; y < HEIGHT; y++) for (int x = 0; x < WIDTH; x++)
        b->pixels[y * WIDTH + x] = ((x / 20 + y / 20 + s->commits) % 2) ? 0xff2878cc : 0xffd8e8f8;
    struct feedback *f = calloc(1, sizeof(*f));
    if (!f) { s->closed = true; return; }
    f->state = s; f->commit = ++s->commits;
    f->proxy = wp_presentation_feedback(s->presentation, s->surface);
    f->next = s->feedbacks; s->feedbacks = f;
    wp_presentation_feedback_add_listener(f->proxy, &feedback_listener, f);
    s->callback = wl_surface_frame(s->surface);
    wl_callback_add_listener(s->callback, &frame_listener, s);
    wl_surface_attach(s->surface, b->proxy, 0, 0);
    wl_surface_damage(s->surface, 0, 0, WIDTH, HEIGHT);
    wl_surface_commit(s->surface);
    b->busy = true; s->frame_ready = false;
}
static void print_samples(struct state *s) {
    if (s->endurance) printf("{\"kind\":\"presentation-batch\",\"batch\":%u,\"elapsed_s\":%.6f,\"presented_total\":%u,", s->batch, monotonic_s() - s->started, s->total_presented);
    else putchar('{');
    printf("\"clock_id\":%" PRIu32 ",\"commits\":%u,\"discarded\":%u,\"pending\":%u,\"frames\":[", s->clock_id, s->commits, s->discarded, s->commits - s->total_presented - s->discarded);
    for (unsigned i = 0; i < s->presented; i++) {
        struct sample *v = &s->samples[i];
        printf("%s{\"commit\":%" PRIu64 ",\"presented_ns\":%" PRIu64 ",\"refresh_ns\":%" PRIu32 ",\"sequence\":%" PRIu64 ",\"flags\":%" PRIu32 "}", i ? "," : "", v->commit, v->timestamp, v->refresh, v->sequence, v->flags);
    }
    puts("]}"); fflush(stdout);
}
int main(int argc, char **argv) {
    struct state s = { .frame_ready = true };
    unsigned seconds = 15;
    if (argc != 1) {
        char *end = NULL;
        errno = 0;
        long value = argc == 3 ? strtol(argv[2], &end, 10) : 0;
        if (argc != 3 || strcmp(argv[1], "--endurance-seconds") || errno ||
            !end || end == argv[2] || *end || value < 1 || value > 86400) {
            fprintf(stderr, "usage: roost-frame-pacer [--endurance-seconds 1..86400]\n"); return 2;
        }
        s.endurance = true; seconds = (unsigned)value;
    }
    s.started = monotonic_s();
    double deadline = s.started + seconds;
    s.display = wl_display_connect(NULL);
    if (!s.display) { perror("Wayland connection"); return 1; }
    struct wl_registry *registry = wl_display_get_registry(s.display);
    wl_registry_add_listener(registry, &registry_listener, &s);
    /* The outer helper also enforces a process deadline during roundtrips. */
    if (wl_display_roundtrip(s.display) < 0 || wl_display_roundtrip(s.display) < 0 ||
        !s.compositor || !s.shm || !s.wm || !s.presentation || !s.clock_known ||
        (s.endurance && !s.idle_manager)) {
        fprintf(stderr, "required native presentation globals/clock unavailable\n"); return 1;
    }
    struct timespec clock_check;
    if (clock_gettime((clockid_t)s.clock_id, &clock_check) != 0 || make_buffers(&s) != 0) { perror("presentation clock/buffers"); return 1; }
    s.surface = wl_compositor_create_surface(s.compositor);
    if (s.endurance) s.inhibitor = zwp_idle_inhibit_manager_v1_create_inhibitor(s.idle_manager, s.surface);
    s.xdg = xdg_wm_base_get_xdg_surface(s.wm, s.surface);
    xdg_surface_add_listener(s.xdg, &xdg_listener, &s);
    s.toplevel = xdg_surface_get_toplevel(s.xdg);
    xdg_toplevel_add_listener(s.toplevel, &top_listener, &s);
    xdg_toplevel_set_title(s.toplevel, "Roost presentation cadence probe");
    xdg_toplevel_set_app_id(s.toplevel, "org.roost.FramePacer");
    xdg_toplevel_set_min_size(s.toplevel, WIDTH, HEIGHT);
    xdg_toplevel_set_max_size(s.toplevel, WIDTH, HEIGHT);
    wl_surface_commit(s.surface);
    bool io_failed = false;
    while (!s.closed && (s.endurance || s.presented < TARGET) && monotonic_s() < deadline) {
        if (wl_display_dispatch_pending(s.display) < 0) { io_failed = true; break; }
        if (s.endurance && ((s.last_presented && monotonic_s() - s.last_presented > 2) ||
            (s.total_presented < TARGET && monotonic_s() - s.started > 15))) {
            fprintf(stderr, "endurance presentation stalled\n"); io_failed = true; break;
        }
        draw(&s);
        int flushed = wl_display_flush(s.display);
        if (flushed < 0 && errno != EAGAIN) { io_failed = true; break; }
        struct pollfd fd = { .fd = wl_display_get_fd(s.display), .events = POLLIN | (flushed < 0 ? POLLOUT : 0) };
        int ready = poll(&fd, 1, 100);
        if (ready < 0 && errno != EINTR) { io_failed = true; break; }
        if (ready > 0 && (fd.revents & (POLLERR | POLLHUP | POLLNVAL))) { io_failed = true; break; }
        if (ready > 0 && (fd.revents & POLLIN) && wl_display_dispatch(s.display) < 0) { io_failed = true; break; }
    }
    int result = 1;
    if (!s.closed && !io_failed && (s.endurance ? (monotonic_s() >= deadline && s.total_presented > 1) : s.presented == TARGET)) {
        if (s.presented) print_samples(&s);
        if (s.endurance) printf("{\"kind\":\"complete\",\"elapsed_s\":%.6f,\"requested_s\":%u,\"presented_total\":%u,\"commits\":%u,\"discarded\":%u,\"pending\":%u}\n", monotonic_s() - s.started, seconds, s.total_presented, s.commits, s.discarded, s.commits - s.total_presented - s.discarded);
        result = 0;
    } else fprintf(stderr, "presentation incomplete: %u/%u, commits %u, discarded %u\n", s.presented, TARGET, s.commits, s.discarded);
    while (s.feedbacks) feedback_remove(s.feedbacks);
    if (s.callback) wl_callback_destroy(s.callback);
    if (s.inhibitor) zwp_idle_inhibitor_v1_destroy(s.inhibitor);
    if (s.idle_manager) zwp_idle_inhibit_manager_v1_destroy(s.idle_manager);
    xdg_toplevel_destroy(s.toplevel); xdg_surface_destroy(s.xdg); wl_surface_destroy(s.surface);
    for (int i = 0; i < 2; i++) wl_buffer_destroy(s.buffers[i].proxy);
    munmap(s.buffers[0].pixels, BYTES * 2);
    wp_presentation_destroy(s.presentation); xdg_wm_base_destroy(s.wm);
    wl_shm_destroy(s.shm); wl_compositor_destroy(s.compositor); wl_registry_destroy(registry);
    wl_display_disconnect(s.display);
    return result;
}
