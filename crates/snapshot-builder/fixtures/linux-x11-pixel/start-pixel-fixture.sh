#!/bin/bash
set -euo pipefail

export DISPLAY=:99
# The restored guest rootfs is read-only; /tmp is the writable tmpfs. HOME and
# XDG_RUNTIME_DIR must live there or the mkdir below dies on the RO rootfs.
export HOME=/tmp/ato-home
export XDG_RUNTIME_DIR=/tmp/ato-xdg

mkdir -p "$HOME" "$XDG_RUNTIME_DIR"
chmod 0700 "$XDG_RUNTIME_DIR"

cleanup() {
  for pid in "${HEALTH_PID:-}" "${VNC_PID:-}" "${APP_PID:-}" "${WM_PID:-}" "${XVFB_PID:-}"; do
    if [ -n "$pid" ]; then
      kill "$pid" 2>/dev/null || true
    fi
  done
}
trap cleanup EXIT INT TERM

Xvfb "$DISPLAY" -screen 0 1280x720x24 -nolisten tcp -noreset &
XVFB_PID=$!

attempt=0
until DISPLAY="$DISPLAY" xdpyinfo >/dev/null 2>&1; do
  kill -0 "$XVFB_PID" 2>/dev/null || exit 1
  attempt=$((attempt + 1))
  [ "$attempt" -le 100 ] || exit 1
  sleep 0.05
done

setxkbmap -display "$DISPLAY" us
openbox >/dev/null 2>&1 &
WM_PID=$!
sleep 0.2

# Capture after the WM is mapped so its own support windows cannot satisfy the
# application readiness check or the framebuffer-change check below.
BEFORE_HASH=$(xwd -display "$DISPLAY" -root -silent | sha256sum | cut -d ' ' -f 1)

xterm \
  -display "$DISPLAY" \
  -class AtoPixelFixture \
  -name ato-pixel-fixture \
  -title "Ato Pixel Stream Fixture" \
  -geometry 80x24+120+80 \
  -e /opt/ato-pixel-fixture/terminal.sh &
APP_PID=$!

WINDOW_ID=$(xdotool search \
  --sync \
  --onlyvisible \
  --pid "$APP_PID" \
  --class AtoPixelFixture | head -n 1)

kill -0 "$APP_PID"
xprop -display "$DISPLAY" -id "$WINDOW_ID" WM_CLASS \
  | grep -q 'AtoPixelFixture'
xwininfo -display "$DISPLAY" -id "$WINDOW_ID" \
  | grep -q 'Map State: IsViewable'

AFTER_HASH=$(xwd -display "$DISPLAY" -root -silent | sha256sum | cut -d ' ' -f 1)
[ "$BEFORE_HASH" != "$AFTER_HASH" ]

# The RFB listener is guest-private. It deliberately has no session password:
# session authorization is generated only after restore and enforced by the
# host gateway. Network policy must prevent direct public access to port 5900.
#
# x11vnc runs under a SERVICE-liveness watchdog, not a process-liveness one:
# the Ready-State artifact is a MEMORY snapshot, and the resumed x11vnc can
# come back WEDGED — the process still exists, so an exit-triggered supervisor
# never fires, but its RFB listener is gone and the host gateway's connect is
# refused. The watchdog therefore probes the actual service (a TCP connect to
# the RFB port) once a second; on failure it kills whatever x11vnc is left and
# starts a fresh one. At build the first pass starts x11vnc normally (the
# health gate below still waits for the listener before the seal); after a
# restore the resumed watchdog detects the dead listener within ~1s and
# rebinds, inside the gateway readiness probe's retry window. Transitions are
# logged to /dev/console for restore forensics.
rfb_listener_up() {
  (exec 3<>/dev/tcp/127.0.0.1/5900) 2>/dev/null || return 1
  exec 3>&- 3<&-
  return 0
}
watch_x11vnc() {
  while kill -0 "$XVFB_PID" 2>/dev/null; do
    if ! rfb_listener_up; then
      echo "[ato-pixel-fixture] rfb listener down; (re)starting x11vnc" >/dev/console 2>/dev/null || true
      pkill -x x11vnc 2>/dev/null || true
      sleep 0.2
      pkill -9 -x x11vnc 2>/dev/null || true
      x11vnc \
        -display "$DISPLAY" \
        -listen 0.0.0.0 \
        -rfbport 5900 \
        -nopw \
        -forever \
        -shared \
        -noclipboard \
        -nosetclipboard \
        -wait 33 \
        -defer 33 \
        -quiet &
      attempt=0
      until rfb_listener_up; do
        attempt=$((attempt + 1))
        [ "$attempt" -le 50 ] || break
        sleep 0.1
      done
      echo "[ato-pixel-fixture] rfb listener state after restart: $(rfb_listener_up && echo up || echo down)" >/dev/console 2>/dev/null || true
    fi
    sleep 1
  done
}
watch_x11vnc &
VNC_PID=$!

ATO_PIXEL_APP_PID="$APP_PID" \
ATO_PIXEL_WINDOW_ID="$WINDOW_ID" \
  /opt/ato-pixel-fixture/health.py &
HEALTH_PID=$!

# x11vnc is deliberately NOT in the fatal wait set — its supervisor loop owns
# its lifecycle so a single post-restore blip never tears the fixture down.
wait -n "$XVFB_PID" "$WM_PID" "$APP_PID" "$HEALTH_PID"
