#define _GNU_SOURCE
#include <wayland-client.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <poll.h>
#include <signal.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include "xdg-shell-client.h"
#include "xdg-system-bell-client.h"
/* Ordinary CI-only bell principal: no provider or shell-control globals. */
static struct wl_display *display;
static struct wl_compositor *compositor;
static struct wl_shm *shm;
static struct xdg_wm_base *wm;
static struct xdg_system_bell_v1 *bell;
static struct wl_surface *surface;
static struct xdg_surface *xdg;
static struct xdg_toplevel *top;
static struct wl_buffer *buffer;
static unsigned configured, rings;
static struct ucred server_peer;
static uint64_t before_ns, after_ns;
static int running=1;
static char socket_path[108];
static void die(const char *message) { perror(message);
  exit(1);
}
static uint32_t id(void *proxy) { return proxy ? wl_proxy_get_id(proxy) : 0;
}
static uint64_t now(void) { struct timespec t;
  if(clock_gettime(CLOCK_MONOTONIC,&t))die("monotonic");
  return (uint64_t)t.tv_sec*1000000000+(uint64_t)t.tv_nsec;
}
static void snapshot(char *text,size_t capacity) {
  int n=snprintf(text,capacity,"{\"pid\":%ld,\"uid\":%ld,\"server_pid\":%ld,\"server_uid\":%ld,\"configured\":%u,\"rings\":%u,\"before_ns\":%llu,\"after_ns\":%llu,\"surface_id\":%u,\"bell_id\":%u,\"xdg_id\":%u,\"buffer_size\":[320,200],\"globals\":{\"compositor\":%u,\"shm\":%u,\"wm\":%u,\"bell\":%u}}\n",(long)getpid(),(long)getuid(),(long)server_peer.pid,(long)server_peer.uid,configured,rings,(unsigned long long)before_ns,(unsigned long long)after_ns,id(surface),id(bell),id(xdg),id(compositor),id(shm),id(wm),id(bell));
  if(n<0||(size_t)n>=capacity)die("snapshot bound");
}
static void ping(void *d,struct xdg_wm_base *w,uint32_t serial){(void)d;
  xdg_wm_base_pong(w,serial);
}
static const struct xdg_wm_base_listener wm_listener={ping};
static void configure(void *d,struct xdg_surface *s,uint32_t serial){(void)d;
  xdg_surface_ack_configure(s,serial);
  if(++configured>32){errno=EOVERFLOW;
    die("configure bound");
  }wl_surface_attach(surface,buffer,0,0);
  wl_surface_damage(surface,0,0,320,200);
  wl_surface_commit(surface);
}
static const struct xdg_surface_listener xdg_listener={configure};
static void top_configure(void *d,struct xdg_toplevel *t,int32_t w,int32_t h,struct wl_array *a){(void)d;
  (void)t;
  (void)w;
  (void)h;
  (void)a;
}
static void top_close(void *d,struct xdg_toplevel *t){(void)d;
  (void)t;
  running=0;
}
static const struct xdg_toplevel_listener top_listener={.configure=top_configure,.close=top_close};
static void buffer_release(void *data,struct wl_buffer *b){(void)data;(void)b;}
static const struct wl_buffer_listener buffer_listener={buffer_release};
static void shm_format(void *data,struct wl_shm *s,uint32_t format){(void)data;
  (void)s;
  (void)format;
}
static const struct wl_shm_listener shm_listener={shm_format};
static void global(void *d,struct wl_registry *r,uint32_t name,const char *interface,uint32_t version){
  (void)d;
  if (!strcmp(interface,"wl_compositor")) {
    if (version < 4 || compositor) die("compositor version/uniqueness");
    compositor = wl_registry_bind(r,name,&wl_compositor_interface,4);
  } else if (!strcmp(interface,"wl_shm")) {
    if (version < 1 || shm) die("shm uniqueness");
    shm = wl_registry_bind(r,name,&wl_shm_interface,1);
    wl_shm_add_listener(shm,&shm_listener,NULL);
  } else if (!strcmp(interface,"xdg_wm_base")) {
    if (version < 1 || wm) die("wm uniqueness");
    wm = wl_registry_bind(r,name,&xdg_wm_base_interface,1);
  } else if (!strcmp(interface,"xdg_system_bell_v1")) {
    if (version < 1 || bell) die("bell uniqueness");
    bell = wl_registry_bind(r,name,&xdg_system_bell_v1_interface,1);
  }
}
static void removed(void *d,struct wl_registry *r,uint32_t n){(void)d;
  (void)r;
  (void)n;
}
static const struct wl_registry_listener registry_listener={global,removed};
static int allowed(const char *s){return !strcmp(s,"status")||!strcmp(s,"ring")||!strcmp(s,"quit");
}
static int control_client(const char *command){
  int fd=socket(AF_UNIX,SOCK_SEQPACKET|SOCK_CLOEXEC,0);
  if(fd<0)die("control socket");
  struct sockaddr_un address={.sun_family=AF_UNIX};
  strcpy(address.sun_path,socket_path);
  if(connect(fd,(struct sockaddr*)&address,sizeof(address)))die("original control connect");
  struct ucred peer;
  socklen_t size=sizeof(peer);
  if(getsockopt(fd,SOL_SOCKET,SO_PEERCRED,&peer,&size)||peer.uid!=getuid())die("server peer UID");
  if(send(fd,command,strlen(command),MSG_NOSIGNAL)!=(ssize_t)strlen(command))die("command send");
  struct pollfd p={.fd=fd,.events=POLLIN};
  if(poll(&p,1,5000)!=1)die("reply deadline");
  char result[4096];
  ssize_t n=recv(fd,result,sizeof(result)-1,0);
  if(n<=0||n==(ssize_t)sizeof(result)-1)die("reply bound");
  result[n]=0;
  char identity[80];
  snprintf(identity,sizeof(identity),"\"pid\":%ld,",(long)peer.pid);
  if(!strstr(result,identity))die("server peer PID");
  fputs(result,stdout);
  close(fd);
  return 0;
}
int main(int argc,char **argv){
  if(!getuid()){errno=EPERM;
    die("ordinary session UID required");
  }
  const char *runtime=getenv("XDG_RUNTIME_DIR");
  if(!runtime||runtime[0]!='/')die("runtime required");
  if(snprintf(socket_path,sizeof(socket_path),"%s/roost-vm-bell.sock",runtime)>=(int)sizeof(socket_path))die("socket path bound");
  if(argc==3&&!strcmp(argv[1],"--command")&&allowed(argv[2]))return control_client(argv[2]);
  if(argc!=1){errno=EINVAL;
    die("fixed arguments");
  }
  pid_t parent=getppid();
  if(prctl(PR_SET_PDEATHSIG,SIGKILL)||getppid()!=parent)die("original parent death guard");
  umask(077);
  int control=socket(AF_UNIX,SOCK_SEQPACKET|SOCK_CLOEXEC,0);
  if(control<0)die("control socket");
  struct sockaddr_un address={.sun_family=AF_UNIX};
  strcpy(address.sun_path,socket_path);
  if(bind(control,(struct sockaddr*)&address,sizeof(address))||listen(control,1))die("unique control bind");
  display=wl_display_connect(NULL);
  if(!display)die("Wayland connection");
  socklen_t peer_size=sizeof(server_peer);
  if(getsockopt(wl_display_get_fd(display),SOL_SOCKET,SO_PEERCRED,&server_peer,&peer_size)||server_peer.uid!=getuid())die("actual Wayland peer credentials");
  struct wl_registry *registry=wl_display_get_registry(display);
  wl_registry_add_listener(registry,&registry_listener,NULL);
  if(wl_display_roundtrip(display)<0||!compositor||!shm||!wm||!bell)die("actual globals");
  xdg_wm_base_add_listener(wm,&wm_listener,NULL);
  surface=wl_compositor_create_surface(compositor);
  xdg=xdg_wm_base_get_xdg_surface(wm,surface);
  xdg_surface_add_listener(xdg,&xdg_listener,NULL);
  top=xdg_surface_get_toplevel(xdg);
  xdg_toplevel_add_listener(top,&top_listener,NULL);
  xdg_toplevel_set_app_id(top,"org.roost.VmBell");
  xdg_toplevel_set_title(top,"Original native bell principal");
  int memory=memfd_create("roost-vm-bell",MFD_CLOEXEC);
  if(memory<0||ftruncate(memory,320*200*4))die("SHM allocation");
  void *pixels=mmap(NULL,320*200*4,PROT_READ|PROT_WRITE,MAP_SHARED,memory,0);
  if(pixels==MAP_FAILED)die("SHM map");
  memset(pixels,0xf0,320*200*4);
  struct wl_shm_pool *pool=wl_shm_create_pool(shm,memory,320*200*4);
  buffer=wl_shm_pool_create_buffer(pool,0,320,200,320*4,WL_SHM_FORMAT_XRGB8888);
  wl_buffer_add_listener(buffer,&buffer_listener,NULL);
  wl_shm_pool_destroy(pool);
  munmap(pixels,320*200*4);
  close(memory);
  wl_surface_commit(surface);
  uint64_t deadline=now()+120000000000;
  while(running&&now()<deadline){
    while(wl_display_prepare_read(display)!=0)if(wl_display_dispatch_pending(display)<0)die("dispatch pending");
    if(wl_display_flush(display)<0&&errno!=EAGAIN)die("flush");
    struct pollfd polls[2]={{.fd=wl_display_get_fd(display),.events=POLLIN},{.fd=control,.events=POLLIN}};
    int ready=poll(polls,2,1000);
    if(ready<0){wl_display_cancel_read(display);
      if(errno==EINTR)continue;
      die("poll");
    }
    if(polls[0].revents&(POLLERR|POLLHUP|POLLNVAL))die("display lost");
    if(polls[0].revents&POLLIN){if(wl_display_read_events(display)<0)die("read events");
    }else wl_display_cancel_read(display);
    if(wl_display_dispatch_pending(display)<0)die("dispatch");
    if(polls[1].revents&POLLIN){int fd=accept4(control,NULL,NULL,SOCK_CLOEXEC);
      if(fd<0)die("accept");
      struct ucred peer;
      socklen_t size=sizeof(peer);
      if(getsockopt(fd,SOL_SOCKET,SO_PEERCRED,&peer,&size)||peer.uid!=getuid())die("client peer UID");
      struct pollfd request={.fd=fd,.events=POLLIN};
      if(poll(&request,1,1000)!=1)die("request deadline");
      char command[32];
      ssize_t n=recv(fd,command,sizeof(command)-1,0);
      if(n<=0||n==(ssize_t)sizeof(command)-1)die("request bound");
      command[n]=0;
      if(!allowed(command)){errno=EINVAL;
        die("fixed command required");
      }
      if(!strcmp(command,"ring")){if(!configured||rings>=16){errno=EOVERFLOW;
          die("mapped/request bound");
        }before_ns=now();
        xdg_system_bell_v1_ring(bell,surface);
        if(wl_display_roundtrip(display)<0)die("bell wire roundtrip");
        after_ns=now();
        rings++;
      }else if(!strcmp(command,"quit"))running=0;
      char result[4096];
      snapshot(result,sizeof(result));
      if(send(fd,result,strlen(result),MSG_NOSIGNAL)!=(ssize_t)strlen(result))die("reply send");
      close(fd);
    }
  }
  if(running){errno=ETIMEDOUT;
    die("principal lifetime bound");
  }xdg_toplevel_destroy(top);
  xdg_surface_destroy(xdg);
  wl_surface_destroy(surface);
  xdg_system_bell_v1_destroy(bell);
  wl_display_flush(display);
  wl_display_disconnect(display);
  close(control);
  unlink(socket_path);
  return 0;
}
