/* Bounded live protocol failures. Server survival is checked by the caller. */
#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <wayland-client.h>
#include "xdg-shell-client-protocol.h"

struct client {
    struct wl_display *display;
    struct wl_compositor *compositor;
    struct wl_shm *shm;
    struct xdg_wm_base *wm;
    unsigned configured;
};
static void ping(void *data, struct xdg_wm_base *wm, uint32_t serial) {
    (void)data; xdg_wm_base_pong(wm, serial);
}
static const struct xdg_wm_base_listener wm_listener = { .ping = ping };
static void global(void *data, struct wl_registry *registry, uint32_t id,
                   const char *name, uint32_t version) {
    struct client *c = data;
    if (!strcmp(name, "wl_compositor"))
        c->compositor = wl_registry_bind(registry, id, &wl_compositor_interface, version < 4 ? version : 4);
    else if (!strcmp(name, "wl_shm"))
        c->shm = wl_registry_bind(registry, id, &wl_shm_interface, 1);
    else if (!strcmp(name, "xdg_wm_base")) {
        c->wm = wl_registry_bind(registry, id, &xdg_wm_base_interface, 1);
        xdg_wm_base_add_listener(c->wm, &wm_listener, c);
    }
}
static void removed(void *data, struct wl_registry *registry, uint32_t id) {
    (void)data; (void)registry; (void)id;
}
static const struct wl_registry_listener registry_listener = { .global = global, .global_remove = removed };
static void configure(void *data, struct xdg_surface *surface, uint32_t serial) {
    (void)surface; (void)serial; ((struct client *)data)->configured++;
    /* Deliberately do not acknowledge: each mode decides its next request. */
}
static const struct xdg_surface_listener surface_listener = { .configure = configure };
static void top_configure(void *data, struct xdg_toplevel *top, int32_t width,
                          int32_t height, struct wl_array *states) {
    (void)data; (void)top; (void)width; (void)height; (void)states;
}
static void close_top(void *data, struct xdg_toplevel *top) { (void)data; (void)top; }
static const struct xdg_toplevel_listener top_listener = { .configure = top_configure, .close = close_top };
static int require_error(struct client *c, const char *mode, const char *name, uint32_t code) {
    int roundtrip = wl_display_roundtrip(c->display);
    int error = wl_display_get_error(c->display);
    if (roundtrip >= 0 || error != EPROTO) {
        fprintf(stderr, "%s: expected protocol rejection, roundtrip=%d error=%d (%s)\n", mode, roundtrip, error, strerror(error)); return 1;
    }
    const struct wl_interface *interface = NULL;
    uint32_t object = 0;
    uint32_t actual = wl_display_get_protocol_error(c->display, &interface, &object);
    if (!interface || strcmp(interface->name, name) || actual != code || !object) {
        fprintf(stderr, "%s: wrong rejection %s code %u object %u\n", mode,
                interface ? interface->name : "none", actual, object); return 1;
    }
    printf("{\"mode\":\"%s\",\"result\":\"protocol-rejected\",\"interface\":\"%s\",\"code\":%u}\n", mode, name, code);
    return 0;
}
int main(int argc, char **argv) {
    if (argc != 2) { fprintf(stderr, "usage: tuna-wayland-adversary MODE\n"); return 2; }
    const char *mode = argv[1];
    struct client c = { .display = wl_display_connect(NULL) };
    if (!c.display) { perror("wl_display_connect"); return 1; }
    struct wl_registry *registry = wl_display_get_registry(c.display);
    wl_registry_add_listener(registry, &registry_listener, &c);
    if (wl_display_roundtrip(c.display) < 0 || !c.compositor || !c.wm || !c.shm) {
        fprintf(stderr, "required globals unavailable\n"); return 1;
    }
    int result = 1;
    if (!strcmp(mode, "malformed-opcode")) {
        uint32_t header[2] = { 1, (8U << 16) | 0xffffU };
        if (write(wl_display_get_fd(c.display), header, sizeof header) != sizeof header) {
            perror("write malformed request"); return 1;
        }
        int roundtrip = wl_display_roundtrip(c.display);
        int error = wl_display_get_error(c.display);
        if (roundtrip >= 0 || (error != EPIPE && error != ECONNRESET && error != EPROTO)) {
            fprintf(stderr, "malformed request was not rejected: roundtrip=%d error=%d\n", roundtrip, error);
        } else if (error == EPROTO) {
            const struct wl_interface *interface = NULL;
            uint32_t object = 0;
            uint32_t code = wl_display_get_protocol_error(c.display, &interface, &object);
            if (interface && !strcmp(interface->name, "wl_display") && object == 1
                && code == WL_DISPLAY_ERROR_INVALID_METHOD) {
                printf("{\"mode\":\"%s\",\"result\":\"connection-rejected\",\"protocol_error\":%u}\n", mode, code);
                result = 0;
            } else fprintf(stderr, "malformed request received an unexpected protocol error\n");
        } else {
            /* Invalid wire opcodes may close transport before an error event. */
            printf("{\"mode\":\"%s\",\"result\":\"connection-rejected\",\"transport_error\":%d}\n", mode, error);
            result = 0;
        }
    } else if (!strcmp(mode, "zero-buffer") || !strcmp(mode, "huge-buffer")) {
        char path[] = "/tmp/tuna-adversary-shm-XXXXXX";
        int fd = mkstemp(path);
        if (fd < 0) { perror("mkstemp"); return 1; }
        unlink(path);
        if (ftruncate(fd, 4096) < 0) { perror("ftruncate"); close(fd); return 1; }
        struct wl_shm_pool *pool = wl_shm_create_pool(c.shm, fd, 4096);
        close(fd);
        int width = !strcmp(mode, "zero-buffer") ? 0 : INT_MAX;
        (void)wl_shm_pool_create_buffer(pool, 0, width, 16, 64, WL_SHM_FORMAT_XRGB8888);
        result = require_error(&c, mode, "wl_shm_pool", WL_SHM_ERROR_INVALID_STRIDE);
    } else {
        struct wl_surface *surface = wl_compositor_create_surface(c.compositor);
        struct xdg_surface *xdg = xdg_wm_base_get_xdg_surface(c.wm, surface);
        xdg_surface_add_listener(xdg, &surface_listener, &c);
        struct xdg_toplevel *top = xdg_surface_get_toplevel(xdg);
        xdg_toplevel_add_listener(top, &top_listener, &c);
        if (!strcmp(mode, "die-mid-transaction")) {
            /* Deliver the new role, then disconnect before its initial commit. */
            if (wl_display_roundtrip(c.display) < 0) return 1;
            printf("{\"mode\":\"%s\",\"result\":\"disconnected-before-commit\"}\n", mode);
            result = 0;
        } else {
            wl_surface_commit(surface);
            for (unsigned i = 0; i < 4 && !c.configured; i++)
                if (wl_display_roundtrip(c.display) < 0) return 1;
            if (!c.configured) { fprintf(stderr, "no actual initial configure\n"); return 1; }
            if (!strcmp(mode, "destroy-parent")) {
                /* Send the destructor but retain its proxy for error identity. */
                wl_proxy_marshal_flags((struct wl_proxy *)c.wm, XDG_WM_BASE_DESTROY, NULL,
                                       wl_proxy_get_version((struct wl_proxy *)c.wm), 0);
                result = require_error(&c, mode, "xdg_wm_base", XDG_WM_BASE_ERROR_DEFUNCT_SURFACES);
            } else if (!strcmp(mode, "invalid-configure")) {
                xdg_surface_ack_configure(xdg, UINT32_MAX);
                result = require_error(&c, mode, "xdg_wm_base", XDG_WM_BASE_ERROR_INVALID_SURFACE_STATE);
            } else if (!strcmp(mode, "never-ack")) {
                /* A client may remain unmapped without blocking others. */
                if (wl_display_roundtrip(c.display) < 0) return 1;
                sleep(1);
                printf("{\"mode\":\"%s\",\"result\":\"configure-left-unacknowledged\",\"configures\":%u}\n", mode, c.configured);
                result = 0;
            } else { fprintf(stderr, "unknown mode: %s\n", mode); result = 2; }
        }
    }
    wl_display_disconnect(c.display);
    return result;
}
