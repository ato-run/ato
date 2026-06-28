# Native-inference GPU (SGLang / CUDA) — Ubuntu NVIDIA runbook

The `nvidia-cuda` profile brings Dockerless **SGLang** (CUDA) acceleration to the
native-inference runtime: a host NVIDIA GPU runs SGLang's server as a plain host
process inside a managed Python venv — no Docker, no Podman, no
nvidia-container-toolkit, no OCI.

> SGLang vs the Vulkan (llama.cpp) path: SGLang is the higher-throughput CUDA
> engine for larger quantized models (e.g. Qwen3-30B-A3B-GPTQ on an RTX A6000);
> the Vulkan path (`--profile nvidia-ubuntu`, see
> `native-inference-vulkan-gpu-runbook.md`) stays the broad-compatibility default.
> The two profiles are independent — installing one never touches the other.

This runbook is the **real E2E** (it requires an Ubuntu 22.04/24.04 + NVIDIA host
with a CUDA-12.8-capable driver; it cannot run on macOS). The code paths are
unit-tested; this validates them on hardware.

## 0. Host requirements

- **Ubuntu 22.04 or 24.04** (the only sets the `nvidia-cuda` profile provisions).
- **An NVIDIA GPU + driver new enough to expose CUDA driver-API ≥ 12.8** (an
  R570-era branch). The SGLang/torch wheels are `cu128`; an older CUDA is a hard
  doctor FAIL (the venv would import-fail at runtime). `provision` does **not**
  install a newer driver mid-run — upgrade the driver first, then re-run.
- **VRAM** sized to the model: Qwen3-30B-A3B-GPTQ fits comfortably in ~44.6 GB
  (validated on a 48 GB A6000). Smaller / more-quantized models need less; the
  doctor's `gpu_vram` row is a WARN hint, never a hard gate.
- **The CUDA JIT toolchain** — `provision` installs it (you do **not** install it
  by hand): SGLang 0.5.x JIT-compiles CUDA kernels at runtime
  (`tvm_ffi → ninja → nvcc → g++`), so the host needs the **CUDA toolkit
  (`nvcc`)**, **`ninja`**, and a **C++ compiler**. The profile lays down
  `cuda-toolkit-12-8` (→ `/usr/local/cuda-12.8`, used as `CUDA_HOME`) +
  `ninja-build` + `build-essential`, from the NVIDIA CUDA apt repo + the Ubuntu
  archive.

## 1. Provision (one step, Dockerless)

```sh
sudo ato runner provision --profile nvidia-cuda     # --dry-run to preview
```

Installs/verifies, in order:

1. **NVIDIA driver** (if missing) — shared with the Vulkan profile, incl. the
   Secure Boot / MOK reboot flow (never auto-disabled; `--resume` after — see
   below).
2. **CUDA runtime gate** — the driver must expose CUDA driver-API ≥ 12.8 (else a
   hard fail with the driver-upgrade remediation).
3. **`python3` + `python3-venv`** (apt) — the managed-venv host interpreter.
4. **CUDA JIT toolchain** — register the NVIDIA CUDA apt repo
   (`cuda-keyring_1.1-1`), then `cuda-toolkit-12-8` + `ninja-build` +
   `build-essential`. Exports `CUDA_HOME=/usr/local/cuda-12.8` (+ `…/bin` on
   PATH) for the venv build / import smoke.
5. **Managed sglang venv** — `uv venv` (CPython 3.12) + a single coherent
   `uv pip install --index-strategy unsafe-best-match --extra-index-url
   https://download.pytorch.org/whl/cu128 "sglang[srt]==0.5.9"`, then an
   `import sglang` smoke (the CUDA "GPU smoke" for this path — it only passes on a
   real CUDA host). Resolves **sglang 0.5.9 + torch 2.9.1+cu128**.

Writes `~/.ato/runner/provision-receipt.json` (`sglang_version`,
`sglang_import_ok`, `cuda_driver_api_version`, `python3_version`,
`max_gpu_vram_bytes`, `gpu_smoke_result`).
**Not installed: Docker, Podman, nvidia-container-toolkit.**

### Resume after a driver reboot

If the driver leg requires a reboot (or Secure Boot MOK enrollment), provision
writes a marker recording **this profile** and exits. Resume the **same** profile
with:

```sh
sudo ato runner provision --resume        # continues nvidia-cuda (from the marker)
```

`--resume` reads the profile from the marker, so you do **not** need to repeat
`--profile nvidia-cuda` (and it will not silently fall back to the Vulkan path).

## 2. Doctor (read-only, Dockerless)

```sh
ato runner doctor --profile nvidia-cuda            # or: --json
```

Expected checks (no `docker` / `nvidia_container_toolkit` / `vulkan_*` rows):

```
os                          ok
secure_boot                 ok|warn
gpu                         ok
nvidia_driver               ok
cuda_runtime                ok          # CUDA driver-API ≥ 12.8 (cu128 floor)
python3                     ok
python_venv                 ok
nvcc                        ok          # CUDA toolkit — sglang JIT needs it
ninja                       ok          # JIT build driver — sglang JIT needs it
sglang_venv                 ok|warn     # ok after provision; warn = not built yet
gpu_vram                    ok|warn     # warn below the recommended headroom
native_inference_cuda_ready ok
```

A **green** doctor (no FAIL rows) implies SGLang can actually JIT-compile its
kernels: the `nvcc` and `ninja` rows FAIL (with a "run provision" hint) when the
toolchain is missing, so green ⇒ the JIT toolchain is present. (The
`native_inference_cuda_ready` host-floor predicate itself is GPU + driver + CUDA
runtime + python/venv; the toolchain is surfaced as its own rows.)

## 3. The capsule

SGLang runs from the **managed** venv — the capsule does not ship the engine. A
minimal `nvidia-cuda` capsule declares the SGLang native-inference target and
allows `CUDA_HOME` through isolation (so the runtime JIT can find `nvcc`):

```toml
[targets.serve]
runtime = "native-inference"
engine = "sglang"
engine_version = "0.5.9"               # the managed sglang[srt] wheel (cu128)
# Model: a CUDA-quantized checkpoint SGLang serves (e.g. Qwen3-30B-A3B-GPTQ via
# gptq_marlin). Use the managed model fields or an explicit model arg per your
# checkpoint; keep server args to the tunable allow-list.
port = 8420

[isolation]
allow_env = ["CUDA_HOME"]              # let the runtime JIT resolve nvcc
```

`provision` exports `CUDA_HOME` for the venv build; at **run** time the capsule
re-supplies it via `allow_env = ["CUDA_HOME"]`, so SGLang's JIT
(`tvm_ffi → ninja → nvcc → g++`) resolves the toolkit.

## 4. Run

```sh
ato run <your-sglang-capsule>          # add --background to detach
```

Expected: the managed venv resolves at `~/.ato/toolchains/sglang-0.5.9/`
(`bin/python`); SGLang's server launches as a host process (no container) on the
declared port; the first request JIT-compiles kernels (one-time, then cached).

## 5. Verify

Readiness: **ato uses a TCP-connect probe** (it waits for the port to accept a
connection); **SGLang serves `GET /health`** once the model is loaded. So a
declared `readiness_probe = { http_get = "/health", port = "<N>" }` lines up with
SGLang's own health endpoint.

```sh
curl http://127.0.0.1:<N>/health                    # 200 once the model is up
curl -X POST http://127.0.0.1:<N>/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"messages":[{"role":"user","content":"hi"}],"max_tokens":16}'   # completion
```

GPU-usage evidence:
1. **SGLang logs** — startup prints the loaded model + quantization (e.g.
   `gptq_marlin`) and the served `/health`. Primary evidence the CUDA engine is up.
2. `nvidia-smi` — the SGLang server process + VRAM usage appear (CUDA clients are
   listed reliably, unlike Vulkan).

**Performance reference** (Qwen3-30B-A3B-GPTQ on an RTX A6000, validated):
TTFT ~64 ms, ~184 tok/s, VRAM ~44.6 / 48 GB.

## 6. Stop — and the teardown gotcha (#778)

SGLang spawns a process tree (the server + a scheduler + a detokenizer). The
#769 `process_group(0)` teardown reaps the **whole group** and frees VRAM — but
only if you signal the **real `ato` binary**, not a wrapper around it.

> **Gotcha:** if you launched via a wrapper such as
> `setsid sudo bash -c "… ato run …"`, then `pgrep -f "ato run"` matches **both**
> the wrapper and the real binary, and a SIGINT delivered to the **wrapper**
> no-ops (the wrapper has already exec'd / is just a shell). Send SIGINT to the
> **actual `ato` binary PID** — then the `process_group(0)` teardown reaps
> SGLang + the scheduler + the detokenizer and releases VRAM.

Prefer the session command, which targets the real process group:

```sh
ato ps                  # runtime=host, ready
ato stop <session-id>   # SIGINT → process group exits; GPU memory released
```

If you must signal by hand, resolve the **binary** PID (not the wrapper):

```sh
# The real binary is the ato process whose argv[0] is the ato executable —
# NOT the `bash -c "… ato run …"` wrapper. Inspect before killing:
pgrep -af 'ato run'     # shows both; pick the one that is the ato binary
kill -INT <ato-binary-pid>
```

### Post-stop verification

Confirm the whole tree is gone and VRAM is freed. Use the `[s]glang` bracket
trick so `pgrep` does not match **itself**:

```sh
pgrep -af '[s]glang.*scheduler'      # empty
pgrep -af '[s]glang.*detokenizer'    # empty
pgrep -af 'ato run'                  # empty (the binary is gone)
nvidia-smi --query-gpu=memory.used --format=csv,noheader   # ~1 MiB (idle)
```

## Status

Code + unit tests landed; **this hardware E2E is pending a real RTX A6000 (or
equivalent CUDA-12.8) host.** Re-validate end to end before relying on it:
`provision --profile nvidia-cuda` → `doctor` green → `ato run` the
Qwen3-30B-A3B-GPTQ capsule → `/health` 200 + a completion + the VRAM figures
above → `ato stop` → post-stop verification clean.
