#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# PocketLLM GPU demo — turnkey, idempotent host setup  (ato-run/ato #788)
# ─────────────────────────────────────────────────────────────────────────────
# Brings a fresh Ubuntu 22.04/24.04 + NVIDIA host to the point where the
#
#     Store → Run → chat
#
# demo for the GPU local-LLM "PocketLLM" (Qwen3-30B-A3B via SGLang) works:
#
#   1. (host)   a NIGHTLY ato binary with the SGLang engine (#775)
#   2. (sudo)   ato runner provision --profile nvidia-cuda
#                 → NVIDIA driver + cuda-toolkit-12-8 + ninja + build-essential
#                 + the managed sglang[srt]==0.5.9 (cu128) venv
#   3. (HUMAN)  ato runner login --headless  → approve the device-flow URL
#   4. cloudflared HOSTNAME tunnel (systemd) for the runner proxy  (#151 raw-IP fix,
#                 and it is OUTBOUND → no inbound firewall / security-group rule)
#   5. ato runner serve (systemd, as ROOT to reuse /root/.ato venv + model)
#   6. open-webui (systemd) chat UI → localhost:30000/v1  + a 2nd cloudflared tunnel
#
# The capsule (ato/pocketllm, repo Koh0920/qwen3-sglang) is ALREADY published —
# this script does not publish anything. The demo entrypoint is the App Page's
# autostart URL (/open is retired with no redirect — ato-pwa#241):
#     https://app.ato.run/a/pocketllm?autostart=1&ref=ato%2Fpocketllm
# and the open-webui tunnel URL is the chat surface.
#
# Every validated command below ran on a real RTX A6000 on 2026-06-25.
#
# IDEMPOTENT: safe to re-run. systemd units use `enable --now`; installs are
# guarded; re-running re-reads the (ephemeral) tunnel URLs and re-points the
# runner + open-webui at them.
#
# It runs the AUTOMATABLE parts and STOPS CLEARLY at the human steps
# (driver-reboot resume, device-flow approval). At the end it prints:
#   • the runner tunnel URL (runner --public-base-url)
#   • the open-webui chat tunnel URL (the chat surface)
#   • the demo entrypoint URL (App Page, autostart)
#
# ── pkill GOTCHA (cost hours, read this) ─────────────────────────────────────
# NEVER run `pkill -f cloudflared` (or any `pkill -f <pattern>` where the pattern
# also occurs in *this* command line) over SSH: pkill -f matches against the FULL
# command line, which includes the SSH session's own shell → it kills your SSH
# session (exit 255). This script ONLY ever stops services via
# `systemctl stop <unit>` (or `pkill cloudflared`, no -f, which matches the
# process NAME). Do the same by hand.
#
# Usage:
#   setup-pocketllm-demo.sh --host <ssh-host-or-alias> [options]
#
#   --host        <user@host | ssh_config_alias>   (REQUIRED) the GPU host
#   --display-name <name>     runner display name        (default: qwen3-a6000)
#   --remote-user <name>      non-root login user on the host
#                             (default: inferred from --host, else "ubuntu")
#   --cuda-home   <path>      CUDA toolkit prefix          (default: /usr/local/cuda-12.8)
#   --ato         <path>      ato binary path on the host  (default: auto-detect)
#   --ssh-key     <path>      ssh -i identity (optional; else your agent / config)
#   --skip-provision          assume the host is already provisioned (skip step 2)
#   --dry-run                 print what each step WOULD do, change nothing
#   -h | --help
#
# Inputs are the SSH host/alias + the enrollment (the operator approves the
# device-flow URL in a browser signed in as the demo account). Everything the
# script can automate, it automates; the two human steps are called out loudly.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

# ── Defaults / inputs ────────────────────────────────────────────────────────
HOST=""
DISPLAY_NAME="qwen3-a6000"
REMOTE_USER=""
CUDA_HOME_DIR="/usr/local/cuda-12.8"
ATO_BIN=""
SSH_KEY=""
SKIP_PROVISION=0
DRY_RUN=0

# Ports / refs (validated values — change only if the capsule changes).
SGLANG_PORT=30000          # SGLang OpenAI-compatible API (loopback)
PROXY_PORT=8080            # ato runner proxy (cloudflared → here) → <slug>.app.ato.run
WEBUI_PORT=8888            # open-webui chat UI (cloudflared → here)
CAPSULE_REF="ato/pocketllm"
# /open is retired with no redirect (ato-pwa#241) — the demo entrypoint is the
# App Page's autostart URL instead: /a/<slug>?autostart=1&ref=<publisher/slug>.
DEMO_APP_URL="https://app.ato.run/a/${CAPSULE_REF##*/}?autostart=1&ref=${CAPSULE_REF//\//%2F}"

# systemd unit names (idempotent: same names every run).
UNIT_CF_RUNNER="ato-cf-tunnel"      # cloudflared → runner proxy (PROXY_PORT)
UNIT_CF_WEBUI="ato-cf-webui"        # cloudflared → open-webui   (WEBUI_PORT)
UNIT_RUNNER="ato-runner-serve"      # ato runner serve (root)
UNIT_WEBUI="ato-webui"              # open-webui serve

# ── Colours / logging ────────────────────────────────────────────────────────
if [ -t 1 ]; then
  RED=$'\033[0;31m'; GREEN=$'\033[0;32m'; YELLOW=$'\033[1;33m'; BLUE=$'\033[0;34m'
  BOLD=$'\033[1m'; NC=$'\033[0m'
else
  RED=""; GREEN=""; YELLOW=""; BLUE=""; BOLD=""; NC=""
fi
step()  { echo; echo "${BLUE}${BOLD}━━━ $* ━━━${NC}"; }
ok()    { echo "${GREEN}✓${NC} $*"; }
info()  { echo "  $*"; }
warn()  { echo "${YELLOW}!${NC} $*"; }
die()   { echo "${RED}✗ $*${NC}" >&2; exit 1; }
human() { echo; echo "${YELLOW}${BOLD}╔══ HUMAN STEP ══════════════════════════════════════════════════╗${NC}";
          echo "${YELLOW}${BOLD}║${NC} $*";
          echo "${YELLOW}${BOLD}╚════════════════════════════════════════════════════════════════╝${NC}"; }

# ── Arg parsing ──────────────────────────────────────────────────────────────
while [ $# -gt 0 ]; do
  case "$1" in
    --host)         HOST="${2:-}"; shift 2 ;;
    --display-name) DISPLAY_NAME="${2:-}"; shift 2 ;;
    --remote-user)  REMOTE_USER="${2:-}"; shift 2 ;;
    --cuda-home)    CUDA_HOME_DIR="${2:-}"; shift 2 ;;
    --ato)          ATO_BIN="${2:-}"; shift 2 ;;
    --ssh-key)      SSH_KEY="${2:-}"; shift 2 ;;
    --skip-provision) SKIP_PROVISION=1; shift ;;
    --dry-run)      DRY_RUN=1; shift ;;
    -h|--help)
      sed -n '2,63p' "$0" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) die "unknown argument: $1 (try --help)" ;;
  esac
done

[ -n "$HOST" ] || die "--host <ssh-host-or-alias> is required (try --help)"

# Infer the remote login user from user@host, else default to ubuntu.
if [ -z "$REMOTE_USER" ]; then
  case "$HOST" in
    *@*) REMOTE_USER="${HOST%%@*}" ;;
    *)   REMOTE_USER="ubuntu" ;;
  esac
fi

# ── SSH plumbing ─────────────────────────────────────────────────────────────
# ServerAliveInterval keeps the long-lived management session up; .201 was known
# to drop on long in-session sleeps, so commands are kept short and idempotent.
SSH_OPTS=(-o ServerAliveInterval=15 -o ServerAliveCountMax=4 -o ConnectTimeout=15)
[ -n "$SSH_KEY" ] && SSH_OPTS+=(-i "$SSH_KEY" -o IdentitiesOnly=yes)

# Run a command on the host. `rsh <cmd...>` runs it as the login user.
rsh() {
  if [ "$DRY_RUN" = 1 ]; then echo "${YELLOW}[dry-run host]${NC} $*"; return 0; fi
  ssh "${SSH_OPTS[@]}" "$HOST" -- "$@"
}
# Run a command on the host with a single string (for shell pipelines / heredocs).
rsh_sh() {
  if [ "$DRY_RUN" = 1 ]; then echo "${YELLOW}[dry-run host sh]${NC} $1"; return 0; fi
  ssh "${SSH_OPTS[@]}" "$HOST" -- "$1"
}
# Like rsh_sh but for CAPTURING output into a variable: in dry-run it returns
# EMPTY on stdout (so the capture stays clean) and notes the command on stderr.
rcap() {
  if [ "$DRY_RUN" = 1 ]; then echo "${YELLOW}[dry-run capture]${NC} $1" >&2; return 0; fi
  ssh "${SSH_OPTS[@]}" "$HOST" -- "$1"
}
# Run a command on the host as root via sudo -n (non-interactive).
rsudo() {
  if [ "$DRY_RUN" = 1 ]; then echo "${YELLOW}[dry-run sudo]${NC} $*"; return 0; fi
  ssh "${SSH_OPTS[@]}" "$HOST" -- "sudo -n $*"
}
rsudo_sh() {
  if [ "$DRY_RUN" = 1 ]; then echo "${YELLOW}[dry-run sudo sh]${NC} $1"; return 0; fi
  ssh "${SSH_OPTS[@]}" "$HOST" -- "sudo -n bash -c $(printf '%q' "$1")"
}

# Install a systemd unit from a heredoc, then enable --now. Idempotent: the unit
# file is overwritten, daemon-reloaded, and (re)started every run.
# $1 = unit name (without .service), $2 = full unit-file content.
install_unit() {
  local unit="$1" body="$2"
  if [ "$DRY_RUN" = 1 ]; then
    echo "${YELLOW}[dry-run unit]${NC} would write /etc/systemd/system/${unit}.service and enable --now:"
    printf '%s\n' "$body" | sed 's/^/      | /'
    return 0
  fi
  # Write the unit file as root, reload, enable+restart.
  printf '%s' "$body" | ssh "${SSH_OPTS[@]}" "$HOST" -- \
    "sudo -n tee /etc/systemd/system/${unit}.service >/dev/null"
  rsudo systemctl daemon-reload
  rsudo systemctl enable "${unit}.service" >/dev/null 2>&1 || true
  rsudo systemctl restart "${unit}.service"
}

# Read the most-recent trycloudflare URL a cloudflared unit logged. The URL is
# EPHEMERAL (new per cloudflared process) → always re-read after (re)start.
read_tunnel_url() {
  local unit="$1" url="" tries=0
  if [ "$DRY_RUN" = 1 ]; then echo "https://<ephemeral>.trycloudflare.com"; return 0; fi
  while [ "$tries" -lt 30 ]; do
    url="$(ssh "${SSH_OPTS[@]}" "$HOST" -- \
      "journalctl -u ${unit}.service --no-pager 2>/dev/null | grep -oE 'https://[a-z0-9-]+\.trycloudflare\.com' | tail -1" \
      2>/dev/null || true)"
    [ -n "$url" ] && { echo "$url"; return 0; }
    tries=$((tries+1)); sleep 2
  done
  return 1
}

echo "${BOLD}PocketLLM GPU demo setup${NC}  →  host=${HOST}  user=${REMOTE_USER}  display-name=${DISPLAY_NAME}"
[ "$DRY_RUN" = 1 ] && warn "DRY-RUN: no changes will be made on the host."

# ─────────────────────────────────────────────────────────────────────────────
step "0/8  Connectivity + host facts"
# ─────────────────────────────────────────────────────────────────────────────
if [ "$DRY_RUN" != 1 ]; then
  rsh true || die "cannot SSH to ${HOST} (check --host / --ssh-key / your ssh config)"
  rsudo true 2>/dev/null \
    || die "passwordless sudo (sudo -n) is required on ${HOST} for provision + systemd units"
fi
ok "SSH + passwordless sudo OK"
# shellcheck disable=SC2016  # $VERSION_ID must expand on the REMOTE host, not locally
OSREL="$(rcap '. /etc/os-release 2>/dev/null && echo "$VERSION_ID"' || true)"
[ -n "$OSREL" ] && info "Ubuntu ${OSREL}"

# Auto-detect the ato binary if not given (prefer one on PATH, then common spots).
if [ -z "$ATO_BIN" ]; then
  ATO_BIN="$(rcap 'command -v ato 2>/dev/null || true')"
  if [ -z "$ATO_BIN" ]; then
    for c in "/home/${REMOTE_USER}/ato/target/release/ato" "/usr/local/bin/ato" "/home/${REMOTE_USER}/.cargo/bin/ato"; do
      if [ "$DRY_RUN" != 1 ] && rsh_sh "test -x $c" 2>/dev/null; then ATO_BIN="$c"; break; fi
    done
  fi
fi
if [ "$DRY_RUN" = 1 ] && [ -z "$ATO_BIN" ]; then ATO_BIN="<auto-detected-on-host>"; fi
[ -n "$ATO_BIN" ] || die "could not find an ato binary on ${HOST}. Build/fetch a NIGHTLY ato first (see the runbook §1), then pass --ato <path>."
ok "ato binary: ${ATO_BIN}"
ATO_VER="$(rcap "$ATO_BIN --version 2>/dev/null" || true)"
[ -n "$ATO_VER" ] && info "version: ${ATO_VER}"
warn "The SGLang engine is NIGHTLY-only (#775). If the version above is not a nightly/0.7 build, rebuild from the nightly branch (runbook §1) — provision will otherwise lack --profile nvidia-cuda."

# ─────────────────────────────────────────────────────────────────────────────
step "1/8  cloudflared (host binary)"
# ─────────────────────────────────────────────────────────────────────────────
# Idempotent: only download if missing. The tunnels run as systemd units (§5).
if [ "$DRY_RUN" != 1 ] && rsh_sh 'test -x /usr/local/bin/cloudflared' 2>/dev/null; then
  ok "cloudflared already installed ($(rcap '/usr/local/bin/cloudflared --version 2>/dev/null | head -1' || echo present))"
else
  info "installing cloudflared → /usr/local/bin/cloudflared"
  rsudo_sh 'curl -fsSL -o /usr/local/bin/cloudflared https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64 && chmod +x /usr/local/bin/cloudflared'
  ok "cloudflared installed"
fi

# ─────────────────────────────────────────────────────────────────────────────
step "2/8  Provision the GPU host  (sudo ato runner provision --profile nvidia-cuda)"
# ─────────────────────────────────────────────────────────────────────────────
# Installs the NVIDIA driver, cuda-toolkit-12-8, ninja-build, build-essential,
# and builds the managed sglang[srt]==0.5.9 (cu128) venv (uv 0.11.24).
# See docs/ops/sglang-cuda-host.md for the full provisioning detail.
if [ "$SKIP_PROVISION" = 1 ]; then
  warn "--skip-provision: assuming the host is already provisioned (skipping)."
else
  # The provision needs CUDA_HOME + nvcc on PATH for the venv build / import smoke.
  PROV_ENV="CUDA_HOME=${CUDA_HOME_DIR} PATH=${CUDA_HOME_DIR}/bin:\$PATH"
  info "running: sudo env ${PROV_ENV} ${ATO_BIN} runner provision --profile nvidia-cuda"
  if [ "$DRY_RUN" = 1 ]; then
    echo "${YELLOW}[dry-run sudo]${NC} env ${PROV_ENV} ${ATO_BIN} runner provision --profile nvidia-cuda"
  else
    set +e
    ssh "${SSH_OPTS[@]}" "$HOST" -- \
      "sudo -n env CUDA_HOME=${CUDA_HOME_DIR} PATH=${CUDA_HOME_DIR}/bin:\$PATH ${ATO_BIN} runner provision --profile nvidia-cuda"
    prov_rc=$?
    set -e 2>/dev/null || true
    if [ "$prov_rc" -ne 0 ]; then
      warn "provision exited ${prov_rc}."
      warn "If it asked for a REBOOT (NVIDIA driver / Secure Boot MOK): that is expected and is a HUMAN step."
      human "Driver install needs a reboot. Do, on the host:
       ${BOLD}sudo reboot${NC}
   then wait for it to come back and RESUME the SAME profile:
       ${BOLD}sudo env CUDA_HOME=${CUDA_HOME_DIR} PATH=${CUDA_HOME_DIR}/bin:\$PATH ${ATO_BIN} runner provision --resume${NC}
   (--resume reads the profile from the marker — do NOT pass --profile again.)

   DRIVER-UPGRADE GOTCHA: if a leftover libnvidia-extra-535 blocks the new driver
   (e.g. 580), clear it then finish the apt install:
       ${BOLD}sudo dpkg --remove --force-depends libnvidia-extra-535 libnvidia-compute-535 2>/dev/null; sudo apt-get -f install -y${NC}
   then re-run this script (it is idempotent and will continue)."
      die "provision incomplete — finish the reboot/resume above, then re-run this script."
    fi
  fi
  ok "provision complete"
  # Read-only confirmation that the CUDA engine floor is satisfied.
  info "doctor (read-only):"
  rsh_sh "$ATO_BIN runner doctor --profile nvidia-cuda 2>&1 | sed 's/^/      /'" || \
    warn "doctor reported issues — review above (native_inference_cuda_ready must be ok before a real run)."
fi

# ─────────────────────────────────────────────────────────────────────────────
step "3/8  Enroll the runner  (HUMAN: approve the device-flow URL)"
# ─────────────────────────────────────────────────────────────────────────────
# `ato runner login --headless` prints a device-flow URL; the operator approves
# it in a browser signed in as the DEMO ACCOUNT. We cannot automate the approval.
# Idempotent: if creds already exist for the login user, we reuse them.
CREDS_USER="/home/${REMOTE_USER}/.ato/runner/credentials.json"
if rsh_sh "test -s ${CREDS_USER}" 2>/dev/null; then
  ok "runner already enrolled (found ${CREDS_USER}) — reusing. (To re-enroll: delete it and re-run.)"
else
  human "Run the device-flow login ON THE HOST yourself (it blocks until approved),
   signed in to the browser as the DEMO ACCOUNT, then approve the printed URL:

       ${BOLD}ssh ${HOST}${NC}
       ${BOLD}${ATO_BIN} runner login --headless --display-name ${DISPLAY_NAME}${NC}

   It prints a URL like  https://app.ato.run/runners/device?code=XXXX  →
   open it in the demo-account browser and approve. When it reports success
   (credentials written to ${CREDS_USER}), re-run THIS script to continue."
  die "runner not enrolled yet — do the device-flow login above, then re-run."
fi

# Copy the enroll creds to /root so the root-run runner reuses the SAME
# /root/.ato venv + model (the runner must run as ROOT for the GPU + model cache).
info "copying enroll creds → /root/.ato/runner/ (root runs the runner)"
rsudo_sh "mkdir -p /root/.ato/runner && cp ${CREDS_USER} /root/.ato/runner/credentials.json && chmod 600 /root/.ato/runner/credentials.json"
ok "root has the runner credentials"

# ─────────────────────────────────────────────────────────────────────────────
step "4/8  cloudflared HOSTNAME tunnel for the runner proxy  (systemd)"
# ─────────────────────────────────────────────────────────────────────────────
# The app-proxy is a Cloudflare Worker that CANNOT fetch a raw-IP origin
# (returns 1003 Direct-IP-Access, #151) — so the runner needs a HOSTNAME. The
# quick tunnel is also OUTBOUND, so NO inbound firewall / security-group rule is
# needed. Ephemeral URL → systemd-managed, re-read after every (re)start.
install_unit "$UNIT_CF_RUNNER" "[Unit]
Description=cloudflared quick tunnel for the ato runner proxy (PocketLLM demo)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/cloudflared tunnel --no-autoupdate --url http://localhost:${PROXY_PORT}
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
"
ok "unit ${UNIT_CF_RUNNER} (re)started"
RUNNER_TUNNEL_URL="$(read_tunnel_url "$UNIT_CF_RUNNER")" \
  || die "could not read the runner tunnel URL from journalctl -u ${UNIT_CF_RUNNER}. Check: sudo systemctl status ${UNIT_CF_RUNNER}"
ok "runner tunnel: ${BOLD}${RUNNER_TUNNEL_URL}${NC}"

# ─────────────────────────────────────────────────────────────────────────────
step "5/8  Serve the runner  (systemd, ROOT, CUDA_HOME set)"
# ─────────────────────────────────────────────────────────────────────────────
# Root → reuses /root/.ato venv + model. --public-base-url = the tunnel hostname
# (so <slug>.app.ato.run routes here). CUDA_HOME survives into the sglang JIT.
# If the tunnel URL changed (ephemeral), this re-points the runner to the new one.
install_unit "$UNIT_RUNNER" "[Unit]
Description=ato runner serve (PocketLLM demo, root, native-inference CUDA)
After=network-online.target ${UNIT_CF_RUNNER}.service
Wants=network-online.target
Requires=${UNIT_CF_RUNNER}.service

[Service]
Type=simple
User=root
Environment=CUDA_HOME=${CUDA_HOME_DIR}
Environment=PATH=${CUDA_HOME_DIR}/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
ExecStart=${ATO_BIN} runner serve --public-base-url ${RUNNER_TUNNEL_URL} --proxy-listen 0.0.0.0:${PROXY_PORT} --max-slots 1
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
"
ok "unit ${UNIT_RUNNER} (re)started → --public-base-url ${RUNNER_TUNNEL_URL}"
sleep 3
if rsh_sh "systemctl is-active ${UNIT_RUNNER}.service" >/dev/null 2>&1; then
  ok "${UNIT_RUNNER} is active"
else
  warn "${UNIT_RUNNER} not active yet — check: sudo systemctl status ${UNIT_RUNNER} ; journalctl -u ${UNIT_RUNNER} -n 50"
fi

# ─────────────────────────────────────────────────────────────────────────────
step "6/8  open-webui chat UI  (venv + systemd)"
# ─────────────────────────────────────────────────────────────────────────────
# A separate venv (uv) running open-webui, pointed at SGLang's OpenAI API on
# localhost:30000. WEBUI_AUTH=false → no login wall for the demo.
WEBUI_VENV="/home/${REMOTE_USER}/webui-venv"
if rsh_sh "test -x ${WEBUI_VENV}/bin/open-webui" 2>/dev/null; then
  ok "open-webui venv already present (${WEBUI_VENV})"
else
  rsh_sh "command -v uv >/dev/null 2>&1" \
    || die "uv not found for ${REMOTE_USER} (provision installs it for root; install uv for ${REMOTE_USER} or run open-webui from a uv-equipped user). See: https://docs.astral.sh/uv/"
  info "creating open-webui venv + installing open-webui (this pulls a large dep set; minutes)"
  rsh_sh "uv venv ${WEBUI_VENV} && uv pip install --python ${WEBUI_VENV}/bin/python open-webui"
  ok "open-webui installed → ${WEBUI_VENV}"
fi

# open-webui systemd unit. Runs as the login user (no GPU needed — it just proxies
# to SGLang's API). WEBUI_URL/CORS are set speculatively for the app-proxy single-
# capsule path (ato-api#152) — UNVERIFIED today; harmless for the 2-surface demo.
install_unit "$UNIT_WEBUI" "[Unit]
Description=open-webui chat UI for PocketLLM (→ SGLang :${SGLANG_PORT})
After=network-online.target ${UNIT_RUNNER}.service
Wants=network-online.target

[Service]
Type=simple
User=${REMOTE_USER}
Environment=OPENAI_API_BASE_URL=http://localhost:${SGLANG_PORT}/v1
Environment=OPENAI_API_KEY=sk-dummy
Environment=WEBUI_AUTH=false
Environment=ENABLE_OLLAMA_API=false
# --- speculative, for the single-capsule app-proxy path (ato-api#152, UNVERIFIED) ---
Environment=CORS_ALLOW_ORIGIN=*
ExecStart=${WEBUI_VENV}/bin/open-webui serve --host 0.0.0.0 --port ${WEBUI_PORT}
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
"
ok "unit ${UNIT_WEBUI} (re)started → :${WEBUI_PORT}  (OPENAI_API_BASE_URL=http://localhost:${SGLANG_PORT}/v1)"

# ─────────────────────────────────────────────────────────────────────────────
step "7/8  cloudflared tunnel for open-webui  (systemd)"
# ─────────────────────────────────────────────────────────────────────────────
install_unit "$UNIT_CF_WEBUI" "[Unit]
Description=cloudflared quick tunnel for open-webui (PocketLLM chat UI)
After=network-online.target ${UNIT_WEBUI}.service
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/cloudflared tunnel --no-autoupdate --url http://localhost:${WEBUI_PORT}
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
"
ok "unit ${UNIT_CF_WEBUI} (re)started"
WEBUI_TUNNEL_URL="$(read_tunnel_url "$UNIT_CF_WEBUI")" \
  || die "could not read the open-webui tunnel URL from journalctl -u ${UNIT_CF_WEBUI}. Check: sudo systemctl status ${UNIT_CF_WEBUI}"
ok "open-webui tunnel: ${BOLD}${WEBUI_TUNNEL_URL}${NC}"

# ─────────────────────────────────────────────────────────────────────────────
step "8/8  Summary — the demo surfaces"
# ─────────────────────────────────────────────────────────────────────────────
cat <<SUMMARY

${GREEN}${BOLD}PocketLLM demo is set up on ${HOST}.${NC}

  ${BOLD}Runner proxy tunnel${NC} (runner --public-base-url; <slug>.app.ato.run routes here):
      ${RUNNER_TUNNEL_URL}

  ${BOLD}Chat UI${NC} (open-webui → SGLang; the chat surface to film/share):
      ${WEBUI_TUNNEL_URL}

  ${BOLD}Demo entrypoint${NC} (open as the DEMO ACCOUNT in a browser):
      ${DEMO_APP_URL}

The demo is TWO surfaces today: the model app_url reached via the App Page's
autostart, and the direct open-webui chat tunnel above. (Single-capsule
"app_url IS the chat UI" is blocked on ato-api#152.)

${YELLOW}Ephemeral-URL reminder:${NC} cloudflared quick-tunnel URLs change whenever the
cloudflared process restarts. The tunnels are systemd units (survive SSH/reboot),
but on restart the URL changes → re-run this script: it re-reads both URLs and
re-points the runner (step 5) + open-webui at them. A named/durable tunnel is the
productionization (out of scope for this live-walkthrough demo).

${YELLOW}Do NOT${NC} 'pkill -f cloudflared' over SSH (it self-kills your session). Manage the
tunnels with: sudo systemctl {status,restart,stop} ${UNIT_CF_RUNNER} ${UNIT_CF_WEBUI}

Verify (on the host):
  ${ATO_BIN} runner doctor --profile nvidia-cuda      # native_inference_cuda_ready ok
  curl -s http://localhost:${SGLANG_PORT}/v1/models    # confirms Qwen3 is the model
  sudo systemctl status ${UNIT_RUNNER} ${UNIT_WEBUI} ${UNIT_CF_RUNNER} ${UNIT_CF_WEBUI}

  Note: "Ollama is running" at the SGLang root is SGLang 0.5.x's Ollama-compat
  page, not a separate Ollama — /v1/models shows the real Qwen3 model.
SUMMARY
ok "done"
