#define _GNU_SOURCE
#include <wayland-client.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include "pointer-constraints-client.h"
#include "relative-pointer-client.h"
#include "layer-shell-client.h"

/* CI-only ordinary session client. No shell control or privileged globals. */
static struct wl_display *display;
static struct wl_compositor *compositor;
static struct wl_shm *shm;
static struct wl_seat *seat;
static struct wl_pointer *pointer;
static struct wl_surface *surface;
static struct zwlr_layer_shell_v1 *layers;
static struct zwlr_layer_surface_v1 *layer;
static struct zwp_pointer_constraints_v1 *constraints;
static struct zwp_relative_pointer_manager_v1 *relative_manager;
static struct zwp_relative_pointer_v1 *relative;
static struct zwp_locked_pointer_v1 *locked;
static struct zwp_confined_pointer_v1 *confined;
static unsigned configured, entered, enter_count, leave_count, motions;
static unsigned locks, unlocks, confines, unconfines, receipts;
static double pointer_x, pointer_y;
static int mode, running = 1;
static char socket_path[108];
static struct { double dx, dy, ux, uy; uint64_t time; } events[128];
static struct { uint32_t serial, width, height; } configure_events[16];

static void die(const char *message) { perror(message); exit(1); }
static uint32_t id(void *proxy) { return proxy ? wl_proxy_get_id(proxy) : 0; }
static void snapshot(char *buffer, size_t capacity) {
 int n = snprintf(buffer, capacity,
 "{\"pid\":%ld,\"uid\":%ld,\"configured\":%u,\"entered\":%u,\"enter_count\":%u,\"leave_count\":%u,\"motions\":%u,\"local\":[%.17g,%.17g],\"locked\":%u,\"unlocked\":%u,\"confined\":%u,\"unconfined\":%u,\"mode\":%d,\"surface_id\":%u,\"pointer_id\":%u,\"relative_id\":%u,\"constraint_id\":%u,\"globals\":{\"compositor\":%u,\"shm\":%u,\"seat\":%u,\"layer_shell\":%u,\"constraints\":%u,\"relative_manager\":%u},\"buffer_size\":[%u,%u],\"configure_events\":[",
 (long)getpid(), (long)getuid(), configured, entered, enter_count, leave_count, motions, pointer_x, pointer_y,
 locks, unlocks, confines, unconfines, mode, id(surface), id(pointer), id(relative), id(locked ? (void *)locked : (void *)confined),
 id(compositor), id(shm), id(seat), id(layers), id(constraints), id(relative_manager), configured ? 128u : 0u, configured ? 128u : 0u);
 if (n < 0 || (size_t)n >= capacity) die("snapshot bound");
 size_t used = (size_t)n;
 for (unsigned i = 0; i < configured; i++) {
  n = snprintf(buffer + used, capacity - used, "%s{\"serial\":%u,\"raw\":[%u,%u],\"chosen\":[128,128]}", i ? "," : "", configure_events[i].serial, configure_events[i].width, configure_events[i].height);
  if (n < 0 || (size_t)n >= capacity - used) die("snapshot configure bound");
  used += (size_t)n;
 }
 n = snprintf(buffer + used, capacity - used, "],\"relative\":[");
 if (n < 0 || (size_t)n >= capacity - used) die("snapshot relative prefix bound");
 used += (size_t)n;
 for (unsigned i = 0; i < receipts; i++) {
  n = snprintf(buffer + used, capacity - used, "%s{\"dx\":%.17g,\"dy\":%.17g,\"dx_unaccel\":%.17g,\"dy_unaccel\":%.17g,\"utime\":%llu}", i ? "," : "", events[i].dx, events[i].dy, events[i].ux, events[i].uy, (unsigned long long)events[i].time);
  if (n < 0 || (size_t)n >= capacity - used) die("snapshot events bound");
  used += (size_t)n;
 }
 if (used + 4 > capacity) die("snapshot suffix bound");
 strcpy(buffer + used, "]}\n");
}
static void relative_motion(void *data, struct zwp_relative_pointer_v1 *p, uint32_t hi, uint32_t lo, wl_fixed_t dx, wl_fixed_t dy, wl_fixed_t ux, wl_fixed_t uy) {
 (void)data; (void)p;
 if (receipts >= 128) { errno = EOVERFLOW; die("actual relative receipt bound"); }
 events[receipts].dx = wl_fixed_to_double(dx); events[receipts].dy = wl_fixed_to_double(dy);
 events[receipts].ux = wl_fixed_to_double(ux); events[receipts].uy = wl_fixed_to_double(uy);
 events[receipts++].time = ((uint64_t)hi << 32) | lo;
}
static const struct zwp_relative_pointer_v1_listener relative_listener = { relative_motion };
static void locked_event(void *d, struct zwp_locked_pointer_v1 *p) { (void)d; (void)p; locks++; }
static void unlocked_event(void *d, struct zwp_locked_pointer_v1 *p) { (void)d; (void)p; unlocks++; }
static const struct zwp_locked_pointer_v1_listener lock_listener = { locked_event, unlocked_event };
static void confined_event(void *d, struct zwp_confined_pointer_v1 *p) { (void)d; (void)p; confines++; }
static void unconfined_event(void *d, struct zwp_confined_pointer_v1 *p) { (void)d; (void)p; unconfines++; }
static const struct zwp_confined_pointer_v1_listener confine_listener = { confined_event, unconfined_event };
static void enter(void *d, struct wl_pointer *p, uint32_t serial, struct wl_surface *s, wl_fixed_t x, wl_fixed_t y) {
 (void)d; (void)p; (void)serial;
 if (s != surface) die("unexpected actual focused surface");
 entered = 1; enter_count++; pointer_x = wl_fixed_to_double(x); pointer_y = wl_fixed_to_double(y);
}
static void leave(void *d, struct wl_pointer *p, uint32_t serial, struct wl_surface *s) { (void)d;(void)p;(void)serial;(void)s;entered=0;leave_count++; }
static void motion(void *d, struct wl_pointer *p, uint32_t time, wl_fixed_t x, wl_fixed_t y) { (void)d;(void)p;(void)time;motions++;pointer_x=wl_fixed_to_double(x);pointer_y=wl_fixed_to_double(y); }
static void button(void *d, struct wl_pointer *p, uint32_t s, uint32_t t, uint32_t b, uint32_t state) { (void)d;(void)p;(void)s;(void)t;(void)b;(void)state; }
static void axis(void *d, struct wl_pointer *p, uint32_t t, uint32_t a, wl_fixed_t v) { (void)d;(void)p;(void)t;(void)a;(void)v; }
static void frame(void *d, struct wl_pointer *p) { (void)d;(void)p; }
static void source(void *d, struct wl_pointer *p, uint32_t s) { (void)d;(void)p;(void)s; }
static void stop_axis(void *d, struct wl_pointer *p, uint32_t t, uint32_t a) { (void)d;(void)p;(void)t;(void)a; }
static void discrete(void *d, struct wl_pointer *p, uint32_t a, int32_t v) { (void)d;(void)p;(void)a;(void)v; }
static const struct wl_pointer_listener pointer_listener = { .enter=enter,.leave=leave,.motion=motion,.button=button,.axis=axis,.frame=frame,.axis_source=source,.axis_stop=stop_axis,.axis_discrete=discrete };
static void capabilities(void *d, struct wl_seat *s, uint32_t caps) {
 (void)d;
 if ((caps & WL_SEAT_CAPABILITY_POINTER) && !pointer) {
  pointer = wl_seat_get_pointer(s); wl_pointer_add_listener(pointer, &pointer_listener, NULL);
 }
}
static void seat_name(void *d, struct wl_seat *s, const char *n) { (void)d;(void)s;(void)n; }
static const struct wl_seat_listener seat_listener = {capabilities,seat_name};
static const struct wl_shm_listener shm_listener;
static void global(void *d, struct wl_registry *registry, uint32_t name, const char *interface, uint32_t version) {
 (void)d;
 #define BIND(type,var,wanted) if (!strcmp(interface,#type)) { if (version < wanted || var) die("global version/uniqueness"); var = wl_registry_bind(registry,name,&type##_interface,wanted); }
 BIND(wl_compositor,compositor,4)
 else if (!strcmp(interface,"wl_shm")) {
  if (version < 1 || shm) die("shm uniqueness");
  shm = wl_registry_bind(registry,name,&wl_shm_interface,1);
  wl_shm_add_listener(shm,&shm_listener,NULL);
 }
 else if (!strcmp(interface,"wl_seat")) {
  if (version < 7 || seat) die("seat version/uniqueness");
  seat = wl_registry_bind(registry,name,&wl_seat_interface,7);
  wl_seat_add_listener(seat,&seat_listener,NULL);
 }
 else BIND(zwlr_layer_shell_v1,layers,4)
 else BIND(zwp_pointer_constraints_v1,constraints,1)
 else BIND(zwp_relative_pointer_manager_v1,relative_manager,1)
 #undef BIND
}
static void removed(void *d, struct wl_registry *r, uint32_t n) { (void)d;(void)r;(void)n; }
static const struct wl_registry_listener registry_listener = {global,removed};
static void shm_format(void *d, struct wl_shm *s, uint32_t format) { (void)d;(void)s;(void)format; }
static const struct wl_shm_listener shm_listener = { shm_format };
static void buffer_release(void *d, struct wl_buffer *b) { (void)d;(void)b; }
static const struct wl_buffer_listener buffer_listener = { buffer_release };
static void configure(void *d, struct zwlr_layer_surface_v1 *l, uint32_t serial, uint32_t w, uint32_t h) {
 (void)d;
 // Layer-shell zero dimensions explicitly leave the choice to the client.
 // Keep the actual requested/buffer geometry fixed at 128; reject every
 // nonzero suggestion that would change this principal's test geometry.
 if ((w && w != 128) || (h && h != 128)) {
  fprintf(stderr,"actual configured dimensions: %u,%u\n",w,h);
  errno = EPROTO; die("actual configured size");
 }
 if (configured >= 16) { errno = EOVERFLOW; die("actual configure receipt bound"); }
 configure_events[configured].serial=serial;configure_events[configured].width=w;configure_events[configured].height=h;
 zwlr_layer_surface_v1_ack_configure(l,serial);
 if (!configured) {
  int fd = memfd_create("tuna-vm-constraint-buffer",MFD_CLOEXEC);
  if (fd < 0 || ftruncate(fd,128*128*4)) die("actual shm buffer");
  uint32_t *pixels = mmap(NULL,128*128*4,PROT_READ|PROT_WRITE,MAP_SHARED,fd,0);
  if (pixels == MAP_FAILED) die("mmap");
  for (int i=0;i<128*128;i++) pixels[i]=0xff203040;
  struct wl_shm_pool *pool=wl_shm_create_pool(shm,fd,128*128*4);
  struct wl_buffer *buffer=wl_shm_pool_create_buffer(pool,0,128,128,128*4,WL_SHM_FORMAT_ARGB8888);
  wl_buffer_add_listener(buffer,&buffer_listener,NULL);
  wl_surface_attach(surface,buffer,0,0);wl_surface_damage(surface,0,0,128,128);
  wl_shm_pool_destroy(pool);munmap(pixels,128*128*4);close(fd);
 }
 configured++;wl_surface_commit(surface);
}
static void closed(void *d, struct zwlr_layer_surface_v1 *l) { (void)d;(void)l;die("actual layer admission closed"); }
static const struct zwlr_layer_surface_v1_listener layer_listener = {configure,closed};
static int allowed(const char *command) {
 return !strcmp(command,"status")||!strcmp(command,"lock")||!strcmp(command,"confine")||!strcmp(command,"destroy")||!strcmp(command,"empty")||!strcmp(command,"restore")||!strcmp(command,"quit");
}
static void execute(const char *command) {
 if (!allowed(command)) die("unknown fixed command");
 if (!strcmp(command,"lock")||!strcmp(command,"confine")) {
  if (mode || !entered) die("constraint requires original pointer focus");
  struct wl_region *region=wl_compositor_create_region(compositor);wl_region_add(region,0,0,64,64);
  if (!strcmp(command,"lock")) {locked=zwp_pointer_constraints_v1_lock_pointer(constraints,surface,pointer,region,ZWP_POINTER_CONSTRAINTS_V1_LIFETIME_PERSISTENT);zwp_locked_pointer_v1_add_listener(locked,&lock_listener,NULL);mode=1;}
  else {confined=zwp_pointer_constraints_v1_confine_pointer(constraints,surface,pointer,region,ZWP_POINTER_CONSTRAINTS_V1_LIFETIME_PERSISTENT);zwp_confined_pointer_v1_add_listener(confined,&confine_listener,NULL);mode=2;}
  wl_region_destroy(region);wl_surface_commit(surface);
 } else if (!strcmp(command,"destroy")) {
  if (!mode) die("no original constraint to destroy");
  if (locked) {zwp_locked_pointer_v1_destroy(locked);locked=NULL;}
  if (confined) {zwp_confined_pointer_v1_destroy(confined);confined=NULL;}
  mode=0;
 } else if (!strcmp(command,"empty")||!strcmp(command,"restore")) {
  if (!mode) die("no original constraint to update");
  struct wl_region *region=wl_compositor_create_region(compositor);
  if (!strcmp(command,"restore")) wl_region_add(region,0,0,64,64);
  if (locked) zwp_locked_pointer_v1_set_region(locked,region);
  if (confined) zwp_confined_pointer_v1_set_region(confined,region);
  wl_region_destroy(region);wl_surface_commit(surface);
 } else if (!strcmp(command,"quit")) running=0;
 if (wl_display_roundtrip(display)<0) die("actual protocol roundtrip");
}
static int connect_control(const char *command) {
 int fd=socket(AF_UNIX,SOCK_SEQPACKET|SOCK_CLOEXEC,0);if(fd<0)die("control socket");
 struct sockaddr_un address={.sun_family=AF_UNIX};strcpy(address.sun_path,socket_path);
 if(connect(fd,(struct sockaddr*)&address,sizeof(address)))die("original control connect");
 struct ucred peer;socklen_t size=sizeof(peer);
 if(getsockopt(fd,SOL_SOCKET,SO_PEERCRED,&peer,&size)||peer.uid!=getuid())die("actual server peer UID");
 if(send(fd,command,strlen(command),MSG_NOSIGNAL)!=(ssize_t)strlen(command))die("fixed command send");
 struct pollfd poller={.fd=fd,.events=POLLIN};if(poll(&poller,1,5000)!=1)die("control response deadline");
 char result[65536];ssize_t n=recv(fd,result,sizeof(result)-1,0);if(n<=0||n==(ssize_t)sizeof(result)-1)die("response bound");result[n]=0;
 char identity[80];snprintf(identity,sizeof(identity),"\"pid\":%ld,",(long)peer.pid);if(!strstr(result,identity))die("original server PID identity");
 fputs(result,stdout);close(fd);return 0;
}
int main(int argc,char **argv) {
 if(!getuid())die("ordinary session UID required");
 const char *runtime=getenv("XDG_RUNTIME_DIR");if(!runtime||runtime[0]!='/')die("session runtime required");
 if(snprintf(socket_path,sizeof(socket_path),"%s/tuna-vm-constraints.sock",runtime)>=(int)sizeof(socket_path))die("socket path bound");
 if(argc==3&&!strcmp(argv[1],"--command")&&allowed(argv[2]))return connect_control(argv[2]);
 if(argc!=1)die("fixed arguments required");
 umask(077);int control=socket(AF_UNIX,SOCK_SEQPACKET|SOCK_CLOEXEC,0);if(control<0)die("control");
 struct sockaddr_un address={.sun_family=AF_UNIX};strcpy(address.sun_path,socket_path);
 if(bind(control,(struct sockaddr*)&address,sizeof(address))||listen(control,1))die("unique session control bind");
 display=wl_display_connect(NULL);if(!display)die("actual Wayland connect");
 struct wl_registry *registry=wl_display_get_registry(display);wl_registry_add_listener(registry,&registry_listener,NULL);
 if(wl_display_roundtrip(display)<0||!compositor||!shm||!seat||!layers||!constraints||!relative_manager)die("actual globals");
 if(wl_display_roundtrip(display)<0||!pointer)die("actual seat pointer");
 surface=wl_compositor_create_surface(compositor);
 relative=zwp_relative_pointer_manager_v1_get_relative_pointer(relative_manager,pointer);zwp_relative_pointer_v1_add_listener(relative,&relative_listener,NULL);
 layer=zwlr_layer_shell_v1_get_layer_surface(layers,surface,NULL,ZWLR_LAYER_SHELL_V1_LAYER_OVERLAY,"tuna-vm-constraints");
 zwlr_layer_surface_v1_add_listener(layer,&layer_listener,NULL);
 zwlr_layer_surface_v1_set_size(layer,128,128);zwlr_layer_surface_v1_set_anchor(layer,ZWLR_LAYER_SURFACE_V1_ANCHOR_TOP|ZWLR_LAYER_SURFACE_V1_ANCHOR_LEFT);
 /* Ordinary layer protocol: extend this test overlay to its anchored edges. */
 zwlr_layer_surface_v1_set_exclusive_zone(layer,-1);zwlr_layer_surface_v1_set_keyboard_interactivity(layer,ZWLR_LAYER_SURFACE_V1_KEYBOARD_INTERACTIVITY_NONE);wl_surface_commit(surface);
 time_t deadline=time(NULL)+300;
 while(running&&time(NULL)<deadline) {
  while(wl_display_prepare_read(display)!=0)if(wl_display_dispatch_pending(display)<0)die("actual dispatch pending");
  if(wl_display_flush(display)<0&&errno!=EAGAIN)die("flush");
  struct pollfd polls[2]={{.fd=wl_display_get_fd(display),.events=POLLIN},{.fd=control,.events=POLLIN}};
  int ready=poll(polls,2,1000);
  if(ready<0){wl_display_cancel_read(display);if(errno==EINTR)continue;die("poll");}
  if(polls[0].revents&(POLLERR|POLLHUP|POLLNVAL))die("actual display connection lost");
  if(polls[0].revents&POLLIN){if(wl_display_read_events(display)<0)die("actual read events");}else wl_display_cancel_read(display);
  if(wl_display_dispatch_pending(display)<0)die("actual event dispatch");
  if(polls[1].revents&POLLIN){
   int fd=accept4(control,NULL,NULL,SOCK_CLOEXEC);if(fd<0)die("accept");
   struct ucred peer;socklen_t size=sizeof(peer);if(getsockopt(fd,SOL_SOCKET,SO_PEERCRED,&peer,&size)||peer.uid!=getuid())die("actual client peer UID");
   struct pollfd request={.fd=fd,.events=POLLIN};if(poll(&request,1,1000)!=1)die("request deadline");
   char command[32];ssize_t n=recv(fd,command,sizeof(command)-1,0);if(n<=0||n==(ssize_t)sizeof(command)-1)die("request bound");command[n]=0;
   execute(command);char result[65536];snapshot(result,sizeof(result));if(send(fd,result,strlen(result),MSG_NOSIGNAL)!=(ssize_t)strlen(result))die("response send");close(fd);
  }
 }
 if(running)die("actual client duration bound");
 zwlr_layer_surface_v1_destroy(layer);wl_surface_destroy(surface);wl_display_flush(display);wl_display_disconnect(display);close(control);unlink(socket_path);return 0;
}
