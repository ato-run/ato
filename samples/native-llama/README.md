# native-llama — Dockerless host-native inference

Runs a [llama.cpp](https://github.com/ggml-org/llama.cpp) `llama-server` as an
Ato-managed **host process** — no Docker, no container, no VM. Both the engine
**and** the model are fetched and cached by Ato; nothing local is required.

## Run

```sh
ato run samples/native-llama
```

Ato will:
1. fetch the pinned llama.cpp release (`engine_version`, e.g. `b9754`) into
   `~/.ato/toolchains/llamacpp-<tag>/` (macOS = Metal, Linux = CPU build),
2. download the model (`model_url`), verify it against `model_sha256`, and
   content-address it in `~/.ato/store/blobs/sha256-<hash>` (re-runs reuse it
   offline; a sha mismatch is a hard error — never a silently wrong model),
3. launch `llama-server -m <cached model> --host 127.0.0.1 --port <N>` as a host
   process and wait for readiness,
4. print the **app_url** — llama.cpp's web UI + OpenAI-compatible API at `/v1`:

```sh
curl http://127.0.0.1:<N>/health
curl -X POST http://127.0.0.1:<N>/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"messages":[{"role":"user","content":"hi"}],"max_tokens":16}'
```

Stop it with `ato stop <session-id>` (it appears in `ato ps` as `runtime=host`).

## Resolution (both default to managed, with local overrides)

- **Engine:** `engine = "llama.cpp"` + `engine_version = "<tag>"` (managed) — or
  `engine_path = "./llama-server"` to use a local binary. `engine_path` wins.
- **Model:** `model_url` + `model_sha256` (managed, content-addressed) — or
  `model = "./model.gguf"` for a local file. `model` wins.

## Increment notes

- Inc1: host launcher lowering (local `engine_path` + `model`).
- Inc2: managed engine auto-fetch (`engine` + `engine_version`).
- Inc3 (this): managed model download + content-addressed cache
  (`model_url` + `model_sha256`; direct http(s) URLs — `hf://` is a later slice).
- GPU provisioning + CUDA engine variants: Inc4.
