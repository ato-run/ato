# Native-inference GPU (Vulkan) — Ubuntu NVIDIA runbook

Inc4 brings Dockerless GPU acceleration to the native-inference runtime: a host
NVIDIA GPU runs a **Vulkan** llama.cpp build as a plain host process — no Docker,
no Podman, no nvidia-container-toolkit, no OCI.

> Why Vulkan, not CUDA: llama.cpp publishes **no Linux CUDA prebuilt** (CUDA
> release assets are Windows-only). It *does* publish `ubuntu-vulkan-x64`, and
> Vulkan runs on NVIDIA via the driver's Vulkan ICD. `engine_variant = "cuda"`
> fails closed on Linux (use `engine_path`, or a future source-build slice).

This runbook is the **real E2E** (it requires an Ubuntu 22.04/24.04 + NVIDIA
host; it cannot run on macOS). The code paths are unit-tested; this validates
them on hardware.

## 1. Diagnose (read-only, Dockerless)

```sh
ato runner doctor --profile nvidia-ubuntu          # or: --json
```

Expected checks (no `docker` / `nvidia_container_toolkit` checks at all):

```
os                            ok
secure_boot                   ok|warn
gpu                           ok
nvidia_driver                 ok
cuda_driver_api               ok|na     # informational only
vulkan_loader                 ok
vulkan_nvidia_device          ok
native_inference_vulkan_ready ok
```

## 2. Provision (Dockerless)

```sh
sudo ato runner provision --profile nvidia-ubuntu   # --dry-run to preview
```

Installs/verifies: NVIDIA driver (if missing) → `vulkan-tools` + `libvulkan1` →
`vulkaninfo --summary` smoke (must show an NVIDIA device). Writes
`~/.ato/runner/provision-receipt.json` with `vulkan_loader_present`,
`vulkan_nvidia_device_visible`, `gpu_smoke_result`.
**Not installed: Docker, Podman, nvidia-container-toolkit.** If Secure Boot is
ON, the MOK/reboot flow is surfaced — it is never auto-disabled; `--resume` after.

## 3. Run a Vulkan native-inference capsule

Use the managed engine **Vulkan** variant + a managed model (Inc3). Either add
`engine_variant = "vulkan"` to `samples/native-llama/capsule.toml`, or make a copy:

```toml
[targets.llama]
runtime = "native-inference"
engine = "llama.cpp"
engine_version = "b9754"
engine_variant = "vulkan"          # ← fetches llama-b9754-bin-ubuntu-vulkan-x64
model_url = "https://huggingface.co/ggml-org/models/resolve/main/tinyllamas/stories15M-q4_0.gguf"
model_sha256 = "66967fbece6dbe97886593fdbb73589584927e29119ec31f08090732d1861739"
port = 8080
```

```sh
ato run samples/native-llama            # background: --background
```

Expected: the engine cache lands at `~/.ato/toolchains/llamacpp-b9754@vulkan/`
(variant-keyed, separate from the CPU build); the model resolves from the Inc3
CAS blob; `llama-server -m <CAS blob> --host 127.0.0.1 --port <N>` runs as a host
process (no container).

## 4. Verify

```sh
curl http://127.0.0.1:<N>/health                    # 200
curl -X POST http://127.0.0.1:<N>/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"messages":[{"role":"user","content":"hi"}],"max_tokens":16}'   # completion
```

GPU-usage evidence (in order of reliability):
1. **llama.cpp logs** — startup prints the selected backend; look for
   `Vulkan` / `ggml_vulkan` / the NVIDIA device name. This is the **primary**
   evidence that the Vulkan backend is in use.
2. `vulkaninfo --summary` — NVIDIA device visible to Vulkan.
3. `nvidia-smi` — the `llama-server` process and/or GPU memory usage may appear.
   Vulkan-compute process visibility in `nvidia-smi` is **best-effort** (it does
   not always list non-CUDA Vulkan clients) — treat (1) as authoritative.

## 5. Stop

```sh
ato ps                 # runtime=host, ready
ato stop <session-id>  # process exits; GPU memory is released
```

## Fail-closed checks (no silent CPU fallback)

The ensure-step dispatches on **variant/platform first**, so each case fails with
its precise reason (never masked behind a generic "needs an NVIDIA GPU"):

- `engine_variant = "vulkan"` on **Linux without full Vulkan readiness**
  (`native_inference_vulkan_ready` = GPU + driver + Vulkan device) → fail closed,
  pointing at `ato runner doctor/provision`. The Vulkan build is fetched, never a
  CPU build — there is no silent fallback.
- `engine_variant = "vulkan"` on **macOS** → explicit error (omit it to use Metal).
- `engine_variant = "cuda"` → fail closed (no managed prebuilt; use `engine_path`).
- `engine_variant = "<unknown>"` → explicit unknown-variant error.
- default / `cpu` / `metal` → no GPU readiness probe at all.

## Status

Code + unit tests landed; **this hardware E2E is pending an Ubuntu NVIDIA host**
(RunPod/Vast/Lambda or bare metal). Record the doctor output, the llama.cpp
Vulkan backend log line, and the completion response when run.
