set -euo pipefail
width=$1 height=$2 rfb_port=$3 display=$4
shift 4
# This helper belongs inside a Run's isolated guest/container. It creates its
# own X server and never accepts DISPLAY, a host socket, or a desktop window ID.
unset XAUTHORITY
export DISPLAY=:$display
for tool in Xtigervnc xdpyinfo xauth; do
  command -v "$tool" >/dev/null || { echo "native_surface_missing_dependency:$tool" >&2; exit 69; }
done
private_dir=$(mktemp -d "${TMPDIR:-/tmp}/ato-pixel.XXXXXXXX")
chmod 0700 "$private_dir"
export XAUTHORITY=$private_dir/Xauthority
: > "$XAUTHORITY"
chmod 0600 "$XAUTHORITY"
cookie=$(od -An -N16 -tx1 /dev/urandom | tr -d " \n")
xauth -f "$XAUTHORITY" add "$DISPLAY" . "$cookie"
unset cookie
vnc_pid= app_pid=
cleanup() {
  trap - EXIT TERM INT
  for pid in "$app_pid" "$vnc_pid"; do
    if [ -n "$pid" ]; then kill "$pid" 2>/dev/null || true; fi
  done
  for pid in "$app_pid" "$vnc_pid"; do
    if [ -n "$pid" ]; then wait "$pid" 2>/dev/null || true; fi
  done
  rm -f "$XAUTHORITY"
  rmdir "$private_dir" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 143' TERM
trap 'exit 130' INT
# TCP X11 is disabled. RFB remains loopback-only, behind the existing gateway.
# Clipboard is disabled in BOTH directions even for a custom RFB client.
Xtigervnc "$DISPLAY" -geometry "${width}x${height}" -depth 24 \
  -rfbport "$rfb_port" -interface 127.0.0.1 -SecurityTypes None \
  -AcceptCutText=0 -SendCutText=0 -AcceptSetDesktopSize=1 \
  -AlwaysShared -nolisten tcp -auth "$XAUTHORITY" &
vnc_pid=$!
for attempt in {1..100}; do
  kill -0 "$vnc_pid" 2>/dev/null || { echo native_surface_server_exited >&2; exit 70; }
  if xdpyinfo -display "$DISPLAY" >/dev/null 2>&1; then break; fi
  if [ "$attempt" = 100 ]; then echo native_surface_display_timeout >&2; exit 70; fi
  sleep 0.05
done
# The argv is data, not evaluated shell text. Window selection belongs to the
# adapter; the private server may present multiple windows of this Run later.
"$@" &
app_pid=$!
wait -n "$vnc_pid" "$app_pid"
