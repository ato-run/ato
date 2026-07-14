#!/bin/bash
set -euo pipefail

export DISPLAY=:99
export HOME=/home/ato
export XDG_RUNTIME_DIR=/run/user/1000

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
VNC_PID=$!

ATO_PIXEL_APP_PID="$APP_PID" \
ATO_PIXEL_WINDOW_ID="$WINDOW_ID" \
  /opt/ato-pixel-fixture/health.py &
HEALTH_PID=$!

wait -n "$XVFB_PID" "$WM_PID" "$APP_PID" "$VNC_PID" "$HEALTH_PID"
