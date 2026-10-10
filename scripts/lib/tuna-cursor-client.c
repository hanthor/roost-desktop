/* A window that sets its pointer cursor the two Wayland ways (#342):
 *   tuna-cursor-client shape NAME   wp_cursor_shape_v1 (no buffer upload)
 *   tuna-cursor-client surface SIZE a SIZE x SIZE wl_pointer cursor surface
 * on every pointer enter, printing one JSON line per enter. Runs until
 * killed; the proof reads what the compositor drew from its state file. */
#define _POSIX_C_SOURCE 200809L
#define _DEFAULT_SOURCE
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <wayland-client.h>
#include "xdg-shell-client-protocol.h"
#include "cursor-shape-v1-client-protocol.h"

struct client {
    struct wl_compositor *compositor;
    struct wl_shm *shm;
    struct wl_seat *seat;
    struct xdg_wm_base *wm;
    struct wp_cursor_shape_manager_v1 *shapes;
    struct wl_pointer *pointer;
    struct wp_cursor_shape_device_v1 *device;
    struct wl_surface *surface, *cursor;
    uint32_t shape;
    int size;
    int configured;
};

static const struct { const char *name; uint32_t shape; } shape_names[] = {
    { "default", WP_CURSOR_SHAPE_DEVICE_V1_SHAPE_DEFAULT },
    { "crosshair", WP_CURSOR_SHAPE_DEVICE_V1_SHAPE_CROSSHAIR },
    { "text", WP_CURSOR_SHAPE_DEVICE_V1_SHAPE_TEXT },
    { "pointer", WP_CURSOR_SHAPE_DEVICE_V1_SHAPE_POINTER },
    { "wait", WP_CURSOR_SHAPE_DEVICE_V1_SHAPE_WAIT },
};

static struct wl_buffer *buffer(struct client *c, int width, int height, uint32_t argb) {
    int stride = width * 4, bytes = stride * height;
    char name[64];
    snprintf(name, sizeof name, "/tuna-cursor-client-%ld", (long)getpid());
    int fd = shm_open(name, O_RDWR | O_CREAT | O_EXCL, 0600);
    if (fd < 0) { perror("shm_open"); exit(1); }
    shm_unlink(name);
    if (ftruncate(fd, bytes) < 0) { perror("ftruncate"); exit(1); }
    uint32_t *pixels = mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (pixels == MAP_FAILED) { perror("mmap"); exit(1); }
    for (int i = 0; i < width * height; i++) pixels[i] = argb;
    munmap(pixels, bytes);
    struct wl_shm_pool *pool = wl_shm_create_pool(c->shm, fd, bytes);
    struct wl_buffer *b = wl_shm_pool_create_buffer(pool, 0, width, height, stride, WL_SHM_FORMAT_ARGB8888);
    wl_shm_pool_destroy(pool);
    close(fd);
    return b;
}

static void enter(void *data, struct wl_pointer *pointer, uint32_t serial,
                  struct wl_surface *surface, wl_fixed_t x, wl_fixed_t y) {
    struct client *c = data;
    (void)surface; (void)x; (void)y;
    if (c->device) {
        wp_cursor_shape_device_v1_set_shape(c->device, serial, c->shape);
        printf("{\"enter\":%u,\"shape\":%u}\n", serial, c->shape);
    } else {
        wl_pointer_set_cursor(pointer, serial, c->cursor, 0, 0);
        printf("{\"enter\":%u,\"surface\":%d}\n", serial, c->size);
    }
    fflush(stdout);
}
static void leave(void *d, struct wl_pointer *p, uint32_t s, struct wl_surface *f) { (void)d; (void)p; (void)s; (void)f; }
static void motion(void *d, struct wl_pointer *p, uint32_t t, wl_fixed_t x, wl_fixed_t y) { (void)d; (void)p; (void)t; (void)x; (void)y; }
static void button(void *d, struct wl_pointer *p, uint32_t s, uint32_t t, uint32_t b, uint32_t st) { (void)d; (void)p; (void)s; (void)t; (void)b; (void)st; }
static void axis(void *d, struct wl_pointer *p, uint32_t t, uint32_t a, wl_fixed_t v) { (void)d; (void)p; (void)t; (void)a; (void)v; }
static void frame(void *d, struct wl_pointer *p) { (void)d; (void)p; }
static void axis_source(void *d, struct wl_pointer *p, uint32_t s) { (void)d; (void)p; (void)s; }
static void axis_stop(void *d, struct wl_pointer *p, uint32_t t, uint32_t a) { (void)d; (void)p; (void)t; (void)a; }
static void axis_discrete(void *d, struct wl_pointer *p, uint32_t a, int32_t v) { (void)d; (void)p; (void)a; (void)v; }
/* Every event of wl_pointer v5, the version bound below. */
static const struct wl_pointer_listener pointer_listener = {
    .enter = enter, .leave = leave, .motion = motion, .button = button, .axis = axis,
    .frame = frame, .axis_source = axis_source, .axis_stop = axis_stop,
    .axis_discrete = axis_discrete,
};

static void ping(void *data, struct xdg_wm_base *wm, uint32_t serial) { (void)data; xdg_wm_base_pong(wm, serial); }
static const struct xdg_wm_base_listener wm_listener = { .ping = ping };
static void global(void *data, struct wl_registry *registry, uint32_t id, const char *name, uint32_t version) {
    struct client *c = data;
    (void)version;
    if (!strcmp(name, "wl_compositor"))
        c->compositor = wl_registry_bind(registry, id, &wl_compositor_interface, 4);
    else if (!strcmp(name, "wl_shm"))
        c->shm = wl_registry_bind(registry, id, &wl_shm_interface, 1);
    else if (!strcmp(name, "wl_seat") && !c->seat)
        c->seat = wl_registry_bind(registry, id, &wl_seat_interface, 5);
    else if (!strcmp(name, "xdg_wm_base")) {
        c->wm = wl_registry_bind(registry, id, &xdg_wm_base_interface, 1);
        xdg_wm_base_add_listener(c->wm, &wm_listener, c);
    } else if (!strcmp(name, "wp_cursor_shape_manager_v1"))
        c->shapes = wl_registry_bind(registry, id, &wp_cursor_shape_manager_v1_interface, 1);
}
static void removed(void *data, struct wl_registry *registry, uint32_t id) { (void)data; (void)registry; (void)id; }
static const struct wl_registry_listener registry_listener = { .global = global, .global_remove = removed };
static void configure(void *data, struct xdg_surface *surface, uint32_t serial) {
    struct client *c = data;
    xdg_surface_ack_configure(surface, serial);
    if (!c->configured++) {
        wl_surface_attach(c->surface, buffer(c, 320, 200, 0xff8f3fd0), 0, 0);
        wl_surface_damage(c->surface, 0, 0, 320, 200);
    }
    wl_surface_commit(c->surface);
}
static const struct xdg_surface_listener surface_listener = { .configure = configure };
static void top_configure(void *d, struct xdg_toplevel *t, int32_t w, int32_t h, struct wl_array *s) { (void)d; (void)t; (void)w; (void)h; (void)s; }
static void close_top(void *data, struct xdg_toplevel *top) { (void)data; (void)top; exit(0); }
static const struct xdg_toplevel_listener top_listener = { .configure = top_configure, .close = close_top };

int main(int argc, char **argv) {
    struct client c = { 0 };
    if (argc != 3 || (strcmp(argv[1], "shape") && strcmp(argv[1], "surface"))) {
        fprintf(stderr, "usage: tuna-cursor-client shape NAME | surface SIZE\n");
        return 2;
    }
    struct wl_display *display = wl_display_connect(NULL);
    if (!display) { perror("wl_display_connect"); return 1; }
    struct wl_registry *registry = wl_display_get_registry(display);
    wl_registry_add_listener(registry, &registry_listener, &c);
    if (wl_display_roundtrip(display) < 0 || !c.compositor || !c.shm || !c.seat || !c.wm) {
        fprintf(stderr, "required globals unavailable\n");
        return 1;
    }
    c.pointer = wl_seat_get_pointer(c.seat);
    wl_pointer_add_listener(c.pointer, &pointer_listener, &c);
    if (!strcmp(argv[1], "shape")) {
        if (!c.shapes) { fprintf(stderr, "wp_cursor_shape_manager_v1 unavailable\n"); return 1; }
        size_t n = sizeof shape_names / sizeof shape_names[0];
        for (size_t i = 0; i < n; i++)
            if (!strcmp(shape_names[i].name, argv[2])) c.shape = shape_names[i].shape;
        if (!c.shape) { fprintf(stderr, "unknown shape %s\n", argv[2]); return 2; }
        c.device = wp_cursor_shape_manager_v1_get_pointer(c.shapes, c.pointer);
    } else {
        c.size = atoi(argv[2]);
        if (c.size < 1 || c.size > 256) { fprintf(stderr, "bad size %s\n", argv[2]); return 2; }
        c.cursor = wl_compositor_create_surface(c.compositor);
        wl_surface_attach(c.cursor, buffer(&c, c.size, c.size, 0xffe01b24), 0, 0);
        wl_surface_damage(c.cursor, 0, 0, c.size, c.size);
        wl_surface_commit(c.cursor);
    }
    c.surface = wl_compositor_create_surface(c.compositor);
    struct xdg_surface *xs = xdg_wm_base_get_xdg_surface(c.wm, c.surface);
    xdg_surface_add_listener(xs, &surface_listener, &c);
    struct xdg_toplevel *top = xdg_surface_get_toplevel(xs);
    xdg_toplevel_add_listener(top, &top_listener, &c);
    xdg_toplevel_set_title(top, "Cursor Probe");
    xdg_toplevel_set_app_id(top, "tuna-cursor-probe");
    wl_surface_commit(c.surface);
    while (wl_display_dispatch(display) >= 0) {
    }
    return 0;
}
