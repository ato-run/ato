# native-llama — Dockerless host-native inference (Inc1)

Runs a [llama.cpp](https://github.com/ggml-org/llama.cpp) `llama-server` as an
Ato-managed **host process** — no Docker, no container, no VM. This is the first
slice of the native-inference runtime: `engine_path` and `model` are **local
paths** (engine fetching and model download come in later increments).

## Prerequisites (provide these two local files)

Inc1 does not fetch anything. Put a llama-server binary and a GGUF model where
the manifest points (the paths are relative to this directory):

1. **Engine** — build or download `llama-server` from llama.cpp and place it at
   `./llama-server` (or edit `engine_path` to an absolute path). On macOS it is
   Metal-accelerated; on Linux+NVIDIA, use a CUDA build.
2. **Model** — any GGUF file at `./model.gguf` (or edit `model`). A small one is
   fine for a smoke, e.g. a Qwen2.5-0.5B-Instruct Q4 GGUF.

```sh
# example layout
samples/native-llama/
  capsule.toml
  llama-server      # your local binary (chmod +x)
  model.gguf        # your local GGUF
```

## Run

```sh
ato run samples/native-llama
```

Ato lowers this to: `./llama-server -m ./model.gguf --host 127.0.0.1 --port <N>`,
waits for readiness on the allocated port, and prints the **app_url**
(`http://127.0.0.1:<N>` — llama.cpp's web UI + OpenAI-compatible API at `/v1`).

Stop it with:

```sh
ato stop <session-id>
```

## Notes

- Missing `engine_path` or `model` fails fast with an explicit error.
- This path is host-native (Tier2): the engine binary and any GPU/driver are
  host-bound, so the run is honestly recorded as not fully hermetic.
