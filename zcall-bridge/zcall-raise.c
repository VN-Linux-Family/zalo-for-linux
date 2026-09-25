/*
 * zcall-raise.c — show the ZaloCall (wine) call window on top, and keep wine
 * in sync with the window manager.
 *
 * On GNOME/mutter a new call window from wine is refused focus (it has no
 * fresh user time): it either opens BELOW the active Zalo window flagged
 * _NET_WM_STATE_DEMANDS_ATTENTION, or — when ZaloCall uses the Windows
 * "minimize, then restore" trick to grab the foreground — stays minimized
 * (_NET_WM_STATE_HIDDEN) because mutter denies the restore. In that second
 * case wine believes the window is restored (IsWindowVisible=1,
 * IsIconic=0), and when the call ends and ZaloCall hides it, wine never
 * unmaps it: a frozen call UI stays on screen until closed by hand.
 *
 * Fix, verified on the real call window: once the window has SETTLED into
 * one of those wrong states, send _NET_ACTIVE_WINDOW with source indication
 * 2 ("pager"). mutter honours it: the window is un-minimized, raised and
 * focused, wine receives the matching events and its state matches the
 * WM again, so the later hide unmaps it normally.
 *
 * Only react to those states and only within SETTLE_MS after the window
 * appears (a later minimize is the user's choice). Never act while the WM is
 * still placing a new window: activating/raising at that moment (an earlier
 * version of this helper) is itself enough to desynchronise wine.
 *
 * Runs for the whole session as a child of the Zalo main process; exits
 * with it (PR_SET_PDEATHSIG) or when the X connection goes away.
 *
 * Build: gcc -O2 -o zcall-raise zcall-raise.c -lX11
 * Debug: ZCALL_RAISE_DEBUG=1 traces decisions on stderr.
 */

#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/prctl.h>
#include <sys/select.h>
#include <time.h>
#include <unistd.h>

#define SETTLE_MS 4000     /* only fix windows this fresh */
#define FIRST_CHECK_MS 300 /* let the WM finish placing a new window */
#define RETRY_MS 700       /* spacing between activations of one window */
#define MAX_ACTIVATIONS 3  /* per appearance (the trick may run after us) */
#define MAX_TRACKED 16

static Atom A_CLIENT_LIST, A_ACTIVE, A_STATE, A_HIDDEN, A_ATTENTION;
static int debug;

static struct {
  Window w;
  long long appeared;
  long long last_activation;
  int activations;
} tracked[MAX_TRACKED];

static void trace(const char *fmt, ...) {
  if (!debug) return;
  va_list ap;
  va_start(ap, fmt);
  vfprintf(stderr, fmt, ap);
  va_end(ap);
}

static long long now_ms(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static int ignore_x_errors(Display *d, XErrorEvent *e) {
  (void)d; (void)e;
  return 0; /* windows vanish between events; never abort on BadWindow */
}

static unsigned long get_xids(Display *d, Window w, Atom prop, Atom type, XID **out) {
  Atom rtype; int fmt; unsigned long n = 0, after; unsigned char *data = NULL;
  *out = NULL;
  if (XGetWindowProperty(d, w, prop, 0, 8192, False, type, &rtype, &fmt, &n, &after, &data) != Success || !data)
    return 0;
  *out = (XID *)data;
  return n;
}

static int contains(const XID *list, unsigned long n, XID x) {
  for (unsigned long i = 0; i < n; i++)
    if (list[i] == x) return 1;
  return 0;
}

/* zalocall.exe class, and a real window: titled or not tiny (wine's helper
 * and IME windows are untitled 1x1). A minimized window can be icon-sized. */
static int is_call_window(Display *d, Window w) {
  XClassHint ch = { 0 };
  int match = 0;
  if (!XGetClassHint(d, w, &ch)) return 0;
  match = (ch.res_class && !strcasecmp(ch.res_class, "zalocall.exe")) ||
          (ch.res_name && !strcasecmp(ch.res_name, "zalocall.exe"));
  if (ch.res_name) XFree(ch.res_name);
  if (ch.res_class) XFree(ch.res_class);
  if (!match) return 0;
  char *name = NULL;
  int titled = XFetchName(d, w, &name) && name && *name;
  if (name) XFree(name);
  XWindowAttributes wa;
  return titled || (XGetWindowAttributes(d, w, &wa) && wa.width > 50 && wa.height > 50);
}

/* mapped by the WM but minimized or refused focus */
static int in_wrong_state(Display *d, Window w) {
  XID *atoms;
  unsigned long n = get_xids(d, w, A_STATE, XA_ATOM, &atoms);
  int wrong = contains(atoms, n, A_HIDDEN) || contains(atoms, n, A_ATTENTION);
  if (atoms) XFree(atoms);
  return wrong;
}

static int find(Window w) {
  for (int i = 0; i < MAX_TRACKED; i++)
    if (tracked[i].w == w) return i;
  return -1;
}

static void track(Window w) {
  int slot = find(w);
  if (slot < 0) {
    slot = 0;
    for (int i = 0; i < MAX_TRACKED; i++)
      if (tracked[i].appeared < tracked[slot].appeared) slot = i; /* free or oldest */
  }
  tracked[slot].w = w;
  tracked[slot].appeared = now_ms();
  tracked[slot].last_activation = 0;
  tracked[slot].activations = 0;
}

static void activate(Display *d, Window root, Window w) {
  XEvent ev;
  memset(&ev, 0, sizeof ev);
  ev.xclient.type = ClientMessage;
  ev.xclient.window = w;
  ev.xclient.message_type = A_ACTIVE;
  ev.xclient.format = 32;
  ev.xclient.data.l[0] = 2; /* source: pager — honoured despite focus-stealing prevention */
  ev.xclient.data.l[1] = CurrentTime;
  XSendEvent(d, root, False, SubstructureRedirectMask | SubstructureNotifyMask, &ev);
  XFlush(d);
}

/* activate tracked window i if it is fresh, settled, not over budget, and
 * in a wrong state */
static void check_slot(Display *d, Window root, int i) {
  long long now = now_ms(), age = now - tracked[i].appeared;
  if (!tracked[i].w || age < FIRST_CHECK_MS || age > SETTLE_MS) return;
  if (tracked[i].activations >= MAX_ACTIVATIONS) return;
  if (tracked[i].last_activation && now - tracked[i].last_activation < RETRY_MS) return;
  if (!in_wrong_state(d, tracked[i].w)) return;
  tracked[i].activations++;
  tracked[i].last_activation = now;
  trace("0x%lx wrong state after %lldms, activate #%d\n", tracked[i].w, age, tracked[i].activations);
  activate(d, root, tracked[i].w);
}

/* true while some tracked window is still inside its settle window */
static int any_settling(void) {
  long long now = now_ms();
  for (int i = 0; i < MAX_TRACKED; i++)
    if (tracked[i].w && now - tracked[i].appeared <= SETTLE_MS) return 1;
  return 0;
}

int main(void) {
  debug = getenv("ZCALL_RAISE_DEBUG") != NULL;
  prctl(PR_SET_PDEATHSIG, SIGTERM);
  if (getppid() == 1) return 0; /* parent already gone */

  Display *d = XOpenDisplay(NULL);
  if (!d) return 1;
  XSetErrorHandler(ignore_x_errors);

  Window root = DefaultRootWindow(d);
  A_CLIENT_LIST = XInternAtom(d, "_NET_CLIENT_LIST", False);
  A_ACTIVE = XInternAtom(d, "_NET_ACTIVE_WINDOW", False);
  A_STATE = XInternAtom(d, "_NET_WM_STATE", False);
  A_HIDDEN = XInternAtom(d, "_NET_WM_STATE_HIDDEN", False);
  A_ATTENTION = XInternAtom(d, "_NET_WM_STATE_DEMANDS_ATTENTION", False);
  XSelectInput(d, root, PropertyChangeMask);

  /* Seed with the current list: windows already on screen are not new. */
  XID *known;
  unsigned long nknown = get_xids(d, root, A_CLIENT_LIST, XA_WINDOW, &known);

  int fd = ConnectionNumber(d);
  for (;;) {
    /* While a window is settling, also poll every 100ms: the WM may have
     * set its state before we selected PropertyChangeMask on it. */
    if (!XPending(d) && any_settling()) {
      fd_set fds;
      FD_ZERO(&fds);
      FD_SET(fd, &fds);
      struct timeval tv = { 0, 100000 };
      if (select(fd + 1, &fds, NULL, NULL, &tv) <= 0) {
        for (int i = 0; i < MAX_TRACKED; i++) check_slot(d, root, i);
        continue;
      }
    }
    XEvent ev;
    XNextEvent(d, &ev); /* returns via the IO error handler (exit) on disconnect */
    if (ev.type != PropertyNotify) continue;
    Window w = ev.xproperty.window;

    if (w == root && ev.xproperty.atom == A_CLIENT_LIST) {
      XID *now;
      unsigned long nnow = get_xids(d, root, A_CLIENT_LIST, XA_WINDOW, &now);
      for (unsigned long i = 0; i < nnow; i++) {
        if (contains(known, nknown, now[i]) || !is_call_window(d, now[i])) continue;
        trace("0x%lx appeared\n", now[i]);
        XSelectInput(d, now[i], PropertyChangeMask);
        track(now[i]);
        /* no action now: the WM is still placing it; its _NET_WM_STATE
         * update (below) tells us where it ended up */
      }
      if (known) XFree(known);
      known = now;
      nknown = nnow;
    } else if (w != root && ev.xproperty.atom == A_STATE) {
      int i = find(w);
      if (i >= 0) check_slot(d, root, i);
    }
  }
}
