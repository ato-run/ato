# PocketLLM GPU demo — turnkey host setup (Store → Open → chat)

Reproduce the GPU local-LLM demo on a **fresh Ubuntu 22.04/24.04 + NVIDIA host**.
"PocketLLM" is **Qwen3-30B-A3B** (MoE) served by **SGLang** (CUDA), opened from
the Store and chatted with through **open-webui**.

> This is a **live-walkthrough demo**, not a persistent product. The cloudflared
> quick-tunnel URLs are ephemeral (they change whenever the tunnel restarts) — a
> named/durable tunnel is the productionization, out of scope here.

- **Script:** [`scripts/setup-pocketllm-demo.sh`](scripts/setup-pocketllm-demo.sh)
  — idempotent; automates the automatable parts and **stops clearly** at the two
  human steps (driver-reboot resume, device-flow approval).
- **Host engine provisioning detail:** [`sglang-cuda-host.md`](sglang-cuda-host.md)
  — the `provision --profile nvidia-cuda` story (driver + CUDA toolkit + the
  managed `sglang[srt]==0.5.9` venv). This runbook builds the **demo** on top of it.
- **Issue:** ato-run/ato#788. The steps below were validated end-to-end on a real
  **RTX A6000** on 2026-06-25 (memory: `native-inference-sglang-engine`, PHASE-5/6).

## What "the demo" is (two surfaces)

The end state is **two URLs** plus the Store entrypoint:

| Surface | What it is | Reached via |
|---------|-----------|-------------|
| **Store → Open** | `app.ato.run/open/ato/pocketllm` → `/v1/launches` routes the native-inference launch to your Connected Runner → redirect to `<slug>.app.ato.run` (the model's app_url) | the demo account, in a browser |
| **Chat UI** | open-webui (OpenAI-compatible chat), pointed at SGLang's `localhost:30000/v1` | its own cloudflared tunnel URL |

The single-capsule **"the app_url IS the chat UI"** (open-webui served *by* the
opened capsule) is **blocked today** (`open-webui` via the app-proxy → 500,
tracked in **ato-api#152**). The working demo is therefore the **two surfaces**
above. The script sets the likely-needed open-webui env (`WEBUI_URL`,
`CORS_ALLOW_ORIGIN=*`) speculatively for #152, marked UNVERIFIED.

## Architecture (why each piece exists)

```
 browser (demo account)
   │  app.ato.run/open/ato/pocketllm
   ▼
 ato-api  /v1/launches ── native-inference ──▶ Connected Runner (this host)   [#150]
   │                                              │ ato run --sandbox (SGLang)
   │  <slug>.app.ato.run  (app-proxy, a CF Worker)│
   ▼                                              ▼
 cloudflared quick tunnel ──────────────▶ runner proxy :8080 ──▶ SGLang :30000
   (HOSTNAME — the app-proxy Worker CANNOT fetch a raw IP: 1003, #151;
    the tunnel is also OUTBOUND, so no inbound firewall/SG rule is needed)

 open-webui :8888  ──▶ SGLang :30000/v1     (separate chat surface)
   ▲
 a 2nd cloudflared quick tunnel  ──▶  the chat UI URL you film/share
```

Key facts that drive the design:

- **The app-proxy is a Cloudflare Worker and cannot fetch a raw-IP origin**
  (returns `1003 Direct IP Access Not Allowed`, **#151**). The runner therefore
  needs a **hostname** — a cloudflared quick tunnel gives it one, and because the
  tunnel is **outbound**, it also removes any inbound firewall / security-group
  requirement.
- **The runner must run as `root`** so it reuses the `/root/.ato` venv + the
  already-downloaded model (provision builds them under root). Enrollment happens
  as your login user, then the credentials are copied to `/root/.ato/runner/`.
- **cloudflared quick-tunnel URLs are ephemeral** (new per cloudflared process).
  The tunnels are systemd units (they survive SSH drops and reboots), but on a
  restart the URL changes → **re-run the script**: it re-reads both URLs and
  re-points the runner (`--public-base-url`) + open-webui at them.

## 0. Prerequisites

- A **fresh Ubuntu 22.04 or 24.04** host with an **NVIDIA GPU** (validated: RTX
  A6000, 48 GB). Driver must be new enough for CUDA driver-API ≥ 12.8 — provision
  installs/upgrades it (with a reboot) if needed.
- **Passwordless `sudo`** for your login user (the script uses `sudo -n` for
  provision and the systemd units).
- **SSH access** from your workstation (a `~/.ssh/config` alias is easiest). If
  the host has many keys offered, pin one with `--ssh-key` (the script adds
  `IdentitiesOnly=yes`).
- The **demo account** browser session (to approve the runner device-flow and to
  drive `/open`).
- The capsule **`ato/pocketllm`** is **already published** (repo
  `Koh0920/qwen3-sglang`) — nothing to publish here.

## 1. A nightly `ato` on the host (SGLang engine is nightly-only, #775)

The SGLang engine landed on **`nightly`** (#775→#783→#784) and is **not** in a
stable release. Put a nightly `ato` on the host one of two ways:

**a) Build from source (most reliable for the exact engine):**

```sh
# on the host
sudo apt-get update && sudo apt-get install -y git build-essential pkg-config libssl-dev
# install rustup if needed: https://rustup.rs
git clone https://github.com/ato-run/ato ~/ato && cd ~/ato
git checkout nightly
cargo build -p cli --release         # → ~/ato/target/release/ato
```

**b) Fetch the latest nightly release artifact:**

```sh
# Inspect the latest nightly tag/assets, then download the linux x86_64 ato:
gh release list --repo ato-run/ato | grep -i nightly | head
# pick the newest v0.7.0-nightly.* tag, then:
gh release download <nightly-tag> --repo ato-run/ato --pattern '*linux*x86_64*' -D ~/ato-dl
# unpack and place the `ato` binary on PATH or pass its path with --ato
```

Confirm it is a nightly/0.7 build: `ato --version`. The setup script also warns
if the detected binary does not look like a nightly build.

## 2. Run the setup script

From your **workstation** (it drives the host over SSH):

```sh
docs/ops/scripts/setup-pocketllm-demo.sh --host <ssh-host-or-alias>
```

Common options:

| Flag | Default | Notes |
|------|---------|-------|
| `--host` | — (**required**) | `user@host` or an ssh_config alias for the GPU host |
| `--display-name` | `qwen3-a6000` | the runner's display name at enrollment |
| `--remote-user` | inferred from `--host`, else `ubuntu` | the non-root login user (whose `~/.ato` holds the enroll creds, and who runs open-webui) |
| `--ato` | auto-detected | path to the nightly `ato` on the host |
| `--cuda-home` | `/usr/local/cuda-12.8` | CUDA toolkit prefix provision lays down |
| `--ssh-key` | your agent / ssh config | `ssh -i` identity (adds `IdentitiesOnly=yes`) |
| `--skip-provision` | off | assume the host is already provisioned (skip step 2) |
| `--dry-run` | off | print what each step **would** do; change nothing |

Run a **`--dry-run` first** to preview the exact commands and unit files. The
script is **idempotent** — safe to re-run; it overwrites the systemd units,
`enable --now`s them, guards installs, and re-reads the ephemeral tunnel URLs.

### What it automates vs. the two human steps

| # | Step | Automated? |
|---|------|-----------|
| 0 | SSH + sudo check, host facts, locate `ato` | ✅ |
| 1 | install `cloudflared` (if missing) | ✅ |
| 2 | `sudo ato runner provision --profile nvidia-cuda` + `doctor` | ✅ — **stops** if a driver **reboot** is required (HUMAN) |
| 3 | `ato runner login --headless` enrollment | **HUMAN** — approve the device-flow URL in the demo-account browser; then re-run the script |
| 3 | copy enroll creds → `/root/.ato/runner/` | ✅ |
| 4 | cloudflared **runner** tunnel (systemd `ato-cf-tunnel`) | ✅ |
| 5 | `ato runner serve` (systemd `ato-runner-serve`, root) | ✅ |
| 6 | open-webui venv + serve (systemd `ato-webui`) | ✅ |
| 7 | cloudflared **open-webui** tunnel (systemd `ato-cf-webui`) | ✅ |
| 8 | print the two tunnel URLs + the `/open` URL | ✅ |

The script **stops clearly** at the two human steps and tells you exactly what to
run, then to re-run itself to continue.

## 3. The exact validated commands (manual fallback)

If you ever run it by hand, these are the commands the script encodes — each one
ran on the real A6000. `<ato>` = the nightly binary; `<user>` = your login user.

**Provision (driver + CUDA toolkit + managed sglang venv):**

```sh
sudo env CUDA_HOME=/usr/local/cuda-12.8 PATH=/usr/local/cuda-12.8/bin:$PATH \
  <ato> runner provision --profile nvidia-cuda
# Driver reboot? → sudo reboot, then resume the SAME profile:
sudo env CUDA_HOME=/usr/local/cuda-12.8 PATH=/usr/local/cuda-12.8/bin:$PATH \
  <ato> runner provision --resume
```

> **Driver-upgrade gotcha:** a leftover `libnvidia-extra-535` can block the new
> driver (e.g. 580):
> `sudo dpkg --remove --force-depends libnvidia-extra-535 libnvidia-compute-535`
> then `sudo apt-get -f install -y`, then re-run.

**Enroll (HUMAN — prints a device-flow URL the operator approves):**

```sh
<ato> runner login --headless --display-name qwen3-a6000
```

> Enrollment is kept a **separate** step (not `provision --enroll`) on purpose:
> the operator must approve the device flow **as the demo account**, and the
> runner is then served **as root** — distinct from the provisioning user. The
> `provision --enroll <name>` shortcut exists but folds enrollment into the
> root-run provision, which is not what the demo wants.

**Run as root (reuse /root/.ato venv + model):**

```sh
sudo mkdir -p /root/.ato/runner
sudo cp /home/<user>/.ato/runner/credentials.json /root/.ato/runner/
```

**cloudflared (hostname tunnel) — systemd units, NOT a bare process:**

```sh
sudo curl -fsSL -o /usr/local/bin/cloudflared \
  https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64
sudo chmod +x /usr/local/bin/cloudflared
# the script installs `ato-cf-tunnel` (→ :8080) and `ato-cf-webui` (→ :8888)
sudo systemctl enable --now ato-cf-tunnel.service ato-cf-webui.service
# read the (ephemeral) URL it picked:
journalctl -u ato-cf-tunnel.service --no-pager | grep -oE 'https://[a-z0-9-]+\.trycloudflare\.com' | tail -1
```

**Serve the runner (systemd `ato-runner-serve`, root, CUDA_HOME set):**

```sh
# ExecStart (as installed by the script):
<ato> runner serve \
  --public-base-url <runner-tunnel-url> \
  --proxy-listen 0.0.0.0:8080 \
  --max-slots 1
```

**open-webui (systemd `ato-webui`):**

```sh
uv venv ~/webui-venv
uv pip install --python ~/webui-venv/bin/python open-webui
# ExecStart (as installed by the script), env:
#   OPENAI_API_BASE_URL=http://localhost:30000/v1
#   OPENAI_API_KEY=sk-dummy   WEBUI_AUTH=false   ENABLE_OLLAMA_API=false
~/webui-venv/bin/open-webui serve --host 0.0.0.0 --port 8888
```

## 4. The demo (after setup is GREEN)

The script prints these at the end:

```
Runner proxy tunnel : https://<rand>.trycloudflare.com   (runner --public-base-url)
Chat UI             : https://<rand>.trycloudflare.com   (open-webui — film this)
Demo entrypoint     : https://app.ato.run/open/ato/pocketllm
```

1. Open **`https://app.ato.run/open/ato/pocketllm`** in the **demo account**
   browser → `/v1/launches` detects native-inference, routes the launch to your
   Connected Runner (`qwen3-a6000`), boots `ato run --sandbox`, and redirects to
   `<slug>.app.ato.run` (the model's app_url) once ready.
2. Show the chat at the **open-webui tunnel URL** — type a prompt, get Qwen3's
   answer (e.g. `25×4 → 100` with a `<think>` trace).

## 5. Verify

On the host:

```sh
<ato> runner doctor --profile nvidia-cuda            # native_inference_cuda_ready: ok
curl -s http://localhost:30000/v1/models             # confirms Qwen3 is the model
curl -s http://localhost:30000/health                # 200 once the model is loaded
sudo systemctl status ato-runner-serve ato-webui ato-cf-tunnel ato-cf-webui
```

Performance reference (Qwen3-30B-A3B-GPTQ on an RTX A6000, validated):
**TTFT ~64 ms, ~184 tok/s, VRAM ~44.6 / 48 GB** — fast because the MoE is
3B-active / 30B-total.

## 6. Gotchas (verbatim — these cost hours)

- **NEVER `pkill -f cloudflared`** (or any `pkill -f <pattern>` where the pattern
  is in the command line) over SSH. `pkill -f` matches the **full** command line,
  which includes the SSH session's own shell → it **kills your SSH session**
  (exit 255). Use **`systemctl stop <unit>`** or `pkill cloudflared` (no `-f`,
  matches the process **name**). The "SSH keeps dropping" symptom on the demo VM
  was 1:1 caused by this.
- **cloudflared quick-tunnel URLs are ephemeral.** They change whenever the
  cloudflared process restarts. The tunnels are systemd units (survive SSH drops
  and reboots), but on a restart the URL changes → **re-run the script** to
  re-read both URLs and re-point the runner + open-webui. (A named/durable tunnel
  is the productionization.)
- **"Ollama is running"** at the SGLang root is **SGLang 0.5.x's Ollama-compat
  page**, not a separate Ollama. Qwen3 is the real model — `/v1/models` confirms
  it. Not a bug.
- **The runner runs as `root`** to reuse `/root/.ato`'s venv + model. If you serve
  it as your login user it will not find the provisioned venv/model.
- **Single-capsule app_url-as-chat-UI is blocked** (open-webui via the app-proxy
  → 500, **ato-api#152**). The demo is two surfaces today. The script sets
  `WEBUI_URL`/`CORS_ALLOW_ORIGIN=*` speculatively for #152 — **UNVERIFIED**.
- **app_url is per-run random** (ato-api `app_proxy.ts` mints a fresh `ulid()` per
  run, keyed on `runId`). It is stable only within an active launch (resume). A
  chat UI cannot hard-code qwen3's app_url across reboots — wire open-webui to
  SGLang's stable `localhost:30000` (as the script does), not to the app_url.

## 7. systemd units the script installs

| Unit | Role | Runs as | Listens / targets |
|------|------|---------|-------------------|
| `ato-cf-tunnel` | cloudflared → runner proxy | (default) | `--url http://localhost:8080` |
| `ato-runner-serve` | `ato runner serve` | **root** | `--proxy-listen 0.0.0.0:8080`, `--public-base-url <runner tunnel>` |
| `ato-webui` | open-webui chat UI | login user | `:8888` → `localhost:30000/v1` |
| `ato-cf-webui` | cloudflared → open-webui | (default) | `--url http://localhost:8888` |

Manage them with `sudo systemctl {status,restart,stop} <unit>` — **never** with
`pkill -f`.

## 8. Teardown gotcha (#778, for reference)

When you stop a `ato run` of an SGLang capsule, signal the **real `ato` binary**,
not a wrapper. If launched via `setsid sudo bash -c "… ato run …"`, then
`pgrep -f "ato run"` matches **both** the wrapper and the binary, and a SIGINT to
the wrapper no-ops. Prefer `ato stop <session-id>` (it targets the real process
group; the `#769` `process_group(0)` teardown reaps SGLang + scheduler +
detokenizer and frees VRAM). See [`sglang-cuda-host.md`](sglang-cuda-host.md) §6.

## Status

The steps were validated end-to-end on a real RTX A6000 (2026-06-25). The demo
VM has since been deleted, so this runbook + script encode the validated path for
re-creation; the script itself has **not** been re-run against a live host since
the VM teardown (re-validate `provision → doctor → /open → chat` on a fresh host).
