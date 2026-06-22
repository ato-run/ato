# local-llm-chat

Run a **local LLM** with one command — **no Docker, no Python setup, no manual
model downloads**. Ato fetches a pinned [llama.cpp](https://github.com/ggml-org/llama.cpp)
engine *and* the GGUF model, verifies them, caches them, and runs `llama-server`
as a host process exposing an **OpenAI-compatible** API.

Model: **Qwen2.5-1.5B-Instruct** (Q4_K_M, ~1.1 GB, Apache-2.0).

## Quick start

```sh
ato run samples/local-llm-chat
```

(From a checkout. Once this capsule is published to the Ato Store / split into its
own repo, it will also run as `ato run <publisher>/local-llm-chat` or
`ato run github.com/<owner>/local-llm-chat` — all resolve to the same managed
engine + model. A bare unscoped name is treated as a search query, not a capsule.)

First run downloads the engine + model (cached afterwards, so re-runs are instant
and work offline). You'll see:

```
1. resolving engine: llama.cpp@b9754
2. downloading engine: macos-arm64 / ubuntu-x64           (your platform)
3. verifying engine cache
4. downloading model: qwen2.5-1.5b-instruct-q4_k_m.gguf   (~1.1 GB, first run only)
5. verifying sha256
6. starting llama-server
7. waiting for /health
8. ready: OpenAI-compatible endpoint
```

Run it in the background and keep your shell:

```sh
ato run samples/local-llm-chat --background
ato ps                       # 🟢 ready, runtime=host
```

## Use it (OpenAI-compatible)

`llama-server` speaks the OpenAI API on the reported port (default `8080`):

```sh
curl http://127.0.0.1:8080/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"messages":[{"role":"user","content":"Explain Dockerless local LLMs in one sentence."}]}'
```

Point any OpenAI-compatible client at `http://127.0.0.1:8080/v1` (any API key — it's
ignored; inference is fully local, nothing leaves your machine).

Stop it and free the resources:

```sh
ato stop --all
```

## GPU acceleration (Linux + NVIDIA)

The default `chat` target is **CPU on Linux / Metal on macOS** — zero setup. On a
Linux NVIDIA host you can run **Vulkan-accelerated** (no Docker, no CUDA toolkit):

```sh
ato runner doctor --profile nvidia-ubuntu          # check readiness
sudo ato runner provision --profile nvidia-ubuntu  # install driver (if needed) + Vulkan runtime
ato run samples/local-llm-chat --target chat-vulkan
```

`engine_variant = "vulkan"` **fails closed** — if the host isn't Vulkan-ready it
errors and points you at `doctor`/`provision` rather than silently running on CPU,
so "did it use the GPU?" is never ambiguous. Verify with `nvidia-smi` (the
`llama-server` process appears) and the engine log (`Vulkan0 : <your GPU>`).

## Requirements

| Target | Needs |
|--------|-------|
| `chat` (default) | ~2 GB free RAM; macOS arm64/x64, or Linux x64. Metal auto-used on macOS. |
| `chat-vulkan` | Linux x64 + NVIDIA GPU (≥ ~2 GB VRAM), driver + Vulkan runtime (`ato runner provision`). |

The model + engine are cached under `~/.ato` (model in the content-addressed store,
engine in the toolchain cache), so only the first run downloads.

## Troubleshooting

| Symptom | Cause / fix |
|---------|-------------|
| `model_sha256` mismatch | The downloaded file didn't match the pinned hash — corrupted/partial download or a changed upstream file. Re-run (partial cache is discarded); the blob is never used unless it verifies. |
| model/engine download failed | Network/HF availability. Re-run; engine + model resume from cache. |
| `engine_variant="vulkan"` not ready | Host isn't Vulkan-ready. Run `ato runner doctor --profile nvidia-ubuntu`, then `provision`. |
| `vulkaninfo` missing / NVIDIA Vulkan ICD missing | `ato runner provision` installs `vulkan-tools` + the loader; the NVIDIA ICD ships with the driver on a bare host. |
| port already in use | Another server holds the port; stop it or set a different `port` in the manifest. |
| process exited before readiness | The engine exited during startup — re-run; on Linux without a sandbox backend, native-inference still runs host-native (no `--dangerously-skip-permissions` needed). |

## How it works

`runtime = "native-inference"` lowers to a **host process** launch: the managed
`llama-server` binary is the command and the model is passed as `-m <model>`. The
engine is a pinned llama.cpp release (`b9754`); the model is a content-addressed
GGUF blob. Nothing runs in a container — it's a Dockerless, host-native LLM runtime.
See `samples/native-llama` for the minimal smoke variant.

## License

This capsule config is part of Ato. The **model** is
[Qwen2.5-1.5B-Instruct](https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF)
under **Apache-2.0**; the **engine** is llama.cpp (MIT). Review each upstream
license for your use.
