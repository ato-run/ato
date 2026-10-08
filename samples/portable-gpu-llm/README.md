# Portable GPU chat

Implementation sample for TODO #141, not a published or real-GPU accepted app.
It uses current `ato.capsule/2` authoring and the portable application v3 bundle,
existing process Adapter, Model Sets and Data Grants. No team/workspace service,
provider key, account-management operation or upstream network permission is
part of the workload. RunPod uses the existing managed Worker and paid-compute
connection; this sample creates no Pod itself.

## Fixed inputs

The existing small-model baseline is Qwen2.5-1.5B-Instruct Q4_K_M, 1,117,320,736
bytes, SHA-256 `6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e`.
Its upstream revision is `91cad51170dc346986eccefdc2dd33a9da36ead9`:
[Qwen model](https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/tree/91cad51170dc346986eccefdc2dd33a9da36ead9).
The software input is the official llama.cpp
[b11429 CUDA 12.8 engine and runtime](https://github.com/ggml-org/llama.cpp/releases/tag/b11429).
The canonical manifests pin sizes and full SHA-256 independently. Their objects
remain outside the `.capsule`, and source URLs are not manifest fields.

To populate an operator-owned source directory before import, explicitly fetch
the three files under the manifest paths. Keep them outside the Capsule source:

```sh
mkdir -p .tmp/llm-inputs/models .tmp/llm-inputs/software
curl --fail --location --continue-at - \
  'https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/91cad51170dc346986eccefdc2dd33a9da36ead9/qwen2.5-1.5b-instruct-q4_k_m.gguf' \
  --output .tmp/llm-inputs/models/qwen.gguf
curl --fail --location --continue-at - \
  'https://github.com/ggml-org/llama.cpp/releases/download/b11429/llama-b11429-bin-ubuntu-cuda-12.8-x64.tar.gz' \
  --output .tmp/llm-inputs/software/llama-cuda.tar.gz
curl --fail --location --continue-at - \
  'https://github.com/ggml-org/llama.cpp/releases/download/b11429/cudart-llama-b11429-bin-ubuntu-cuda-12.8-x64.tar.gz' \
  --output .tmp/llm-inputs/software/cuda-runtime.tar.gz
```

The shared `ato model-set import` command verifies each manifest/object and
enforces an explicit cache ceiling; see
[local import](../../docs/guides/connected-runner-self-host.md#import-an-existing-model-set-offline).
Hosted Runs require the same manifests/objects registered through the existing
owner-scoped Model Set/Data Grant path. Copying files to this directory alone
does not register hosted data or authorize a paid launch. Local CLI host/GPU
admission remains refused until its device-allocation gate is implemented.

## Execution and history

The pinned Python process extracts verified software into Run scratch, requires
a CUDA device, starts one engine over a private Unix socket and requests all
layers on that GPU with automatic fitting disabled. `/health` returns the
declared ready body only after the engine answers and its log confirms complete
GPU layer offload. Unknown/missing CUDA or partial offload fails closed. There
is no CPU D or fallback. No workload opens a second TCP listener or downloads
models. The Runner owns hardware admission, isolation, Run stop and Pod cleanup.

Chat streams text from the engine. One generation runs at a time. Request IDs
are idempotent across saved history: submitting the same ID/prompt returns the
existing result; a changed prompt conflicts. Cancellation shuts down the
inference connection and preserves partial text. It cancels the generation;
the loaded model remains until the common Run is stopped.

`ATO_OUTPUT_DIR/chat-history.json` is an atomic, bounded checkpoint containing
prompts, responses and completion/cancellation state, never weights. The common
output-save path persists it as an Instance Asset after confirmed stop. Export
history downloads the same schema while the Run is live. A subsequent Run can
restore exactly one owner-selected input Asset named `chat-history.json`
through its Data Grant. Interrupted generations become `interrupted` and are
never automatically repeated. Automatic next-Run Asset selection and a full
same-Instance restart acceptance are not supplied by this sample.

## Verification boundary

`tests/test_chat.py` uses an HTTP engine fixture over a real Unix socket to test
streaming, cancellation disconnect, idempotency, restart checkpoints, bounds
and readiness gating. It does not perform inference or prove CUDA compatibility.
The real acceptance still requires an authorized RunPod GPU, cost ceiling,
model registration, full readiness receipt, nonempty generated text, live
stream/cancel, history save/restore and confirmed owned-Pod deletion.
