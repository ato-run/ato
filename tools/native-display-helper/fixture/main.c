// Independent GUI app fixture. No TigerVNC, shell wrapper, Ato agent or
// network server enters its image. The authored expected frame is green.
#include <X11/Xlib.h>
#include <stdlib.h>
#include <unistd.h>
int main(int argc, char **argv) {
  Display *d = XOpenDisplay(NULL);
  if (!d) return 69;
  int s = DefaultScreen(d), w = DisplayWidth(d,s), h = DisplayHeight(d,s);
  XSetWindowAttributes attrs = {.override_redirect=True,.background_pixel=0x112233};
  Window win = XCreateWindow(d,RootWindow(d,s),0,0,w,h,0,CopyFromParent,InputOutput,CopyFromParent,CWOverrideRedirect|CWBackPixel,&attrs);
  XSelectInput(d,win,ExposureMask|KeyPressMask);
  char empty[1]={0}; Pixmap bitmap=XCreateBitmapFromData(d,win,empty,1,1);
  XColor black={0}; Cursor cursor=XCreatePixmapCursor(d,bitmap,bitmap,&black,&black,0,0);
  XDefineCursor(d,win,cursor);
  XMapRaised(d,win); XSetInputFocus(d,win,RevertToParent,CurrentTime);
  GC gc=XCreateGC(d,win,0,NULL);
  unsigned long color=0x112233; int n=0;
  XSync(d,False);
  if (argc > 1 && argv[1][0] >= '0' && argv[1][0] <= '9') { usleep(atoi(argv[1])*1000); return 0; }
  for (;;) {
    XEvent event; XNextEvent(d,&event);
    if(event.type==KeyPress) { n++; color=(n==1?0x336699:0x993366); }
    if(event.type==Expose || event.type==KeyPress) {
      XSetForeground(d,gc,color); XFillRectangle(d,win,gc,0,0,w,h); XSync(d,False);
    }
  }
}
