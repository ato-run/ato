// Independent GUI app fixture. No TigerVNC, shell wrapper, Ato agent or
// network server enters its image. The authored expected frame is green.
#include <X11/Xlib.h>
#include <stdlib.h>
#include <unistd.h>
#include <dirent.h>
#include <stdio.h>
#include <string.h>
// Optional ordinary file exchange; a successful key frame depends on the
// actual input read, failed write, and output copy when these are declared.
static int exchange_files(void) {
  const char *inputs=getenv("ATO_INPUT_ASSETS_DIR"), *outputs=getenv("ATO_OUTPUT_DIR");
  if (!inputs && !outputs) return 0;
  if (!inputs || !outputs) return 1;
  DIR *dir=opendir(inputs); if (!dir) return 1;
  struct dirent *entry; char source[1024]={0}, target[1024];
  while ((entry=readdir(dir))) {
    if (entry->d_name[0]!='.') {
      snprintf(source,sizeof source,"%s/%s/seed.json",inputs,entry->d_name); break;
    }
  }
  closedir(dir);
  if (!source[0]) return 1;
  FILE *file=fopen(source,"rb"); if (!file) return 1;
  unsigned char bytes[4096]; size_t count=fread(bytes,1,sizeof bytes,file);
  int bad=ferror(file) || count==0 || !feof(file); fclose(file); if (bad) return 1;
  file=fopen(source,"ab"); if (file) { fclose(file); return 1; }
  snprintf(target,sizeof target,"%s/result.json",outputs);
  file=fopen(target,"wb"); if (!file) return 1;
  bad=fwrite(bytes,1,count,file)!=count; bad|=fclose(file)!=0;
  return bad;
}
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
    if(event.type==KeyPress) {
      if(n==0 && exchange_files()) return 71;
      n++; color=(n==1?0x336699:0x993366);
    }
    if(event.type==Expose || event.type==KeyPress) {
      XSetForeground(d,gc,color); XFillRectangle(d,win,gc,0,0,w,h); XSync(d,False);
    }
  }
}
