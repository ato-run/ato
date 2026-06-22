# native-llama — Dockerless host-native inference

Runs a [llama.cpp](https://github.com/ggml-org/llama.cpp) `llama-server` as an
Ato-managed **host process** — no Docker, no container, no VM. The engine binary
is **auto-fetched** by pinned build tag; you only provide a GGUF model.

## Provide a model

Put any GGUF file at `./model.gguf` (or edit `model`). A small one is fine for a
smoke, e.g. a TinyLlama / Qwen2.5-0.5B Q4 GGUF.

## Run

```sh
ato run samples/native-llama
```

Ato will:
1. fetch the pinned llama.cpp release (`engine_version`, e.g. `b9754`) into
   `~/.ato/toolchains/llamacpp-<tag>/` (macOS = Metal, Linux = CPU build),
2. launch `llama-server -m ./model.gguf --host 127.0.0.1 --port <N>` as a host
   process and wait for readiness,
3. print the **app_url** — llama.cpp's web UI + OpenAI-compatible API at `/v1`:

```sh
curl http://127.0.0.1:<N>/health
curl -X POST http://127.0.0.1:<N>/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"messages":[{"role":"user","content":"hi"}],"max_tokens":16}'
```

Stop it with `ato stop <session-id>` (it appears in `ato ps` as `runtime=host`).

## Engine resolution

- **Managed (default here):** `engine = "llama.cpp"` + `engine_version = "<tag>"`
  → Ato fetches + caches `llama-server` (re-runs reuse the cache).
- **Local override:** set `engine_path = "./llama-server"` (drop `engine_version`)
  to use a binary you provide. `engine_path` always wins.

## Increment notes

- Inc1: host launcher lowering (`engine_path` + `model`, local paths).
- Inc2 (this): managed engine auto-fetch (`engine` + `engine_version`).
- Inc3 (next): model download/cache (so `model` can be an `hf://`/URL ref).
- GPU provisioning + CUDA engine variants: Inc4.
