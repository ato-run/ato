# b11429 regression evidence

All file digests and binary/source origins are in `provenance.json`.
Upstream is **b11429**, commit `d81235049384534c167caea52b85a694f6103d14`.

- `trial2.stderr.txt` / `trial2.diagnostics.json`: exact saved RunPod trial-2
  Asset bytes, retrieved after Pod deletion. Engine HTTP200, verbosity3,
  stderr1164 bytes, stdout0, complete collection, no layer placement evidence.
- `cpu-negative.jsonl` / `cpu-negative.audit.json`: actual shipped CUDA engine
  loading the digest-pinned Qwen GGUF on an existing CPU host in Ubuntu24.04.
  HTTP200, all29 layer assignments CPU, no GPU execution. The owned container
  was stopped and removed. This is a negative audit, not a sample CPU fallback.
  The JSONL fixture is a **verbatim ordered excerpt** (no rewritten messages)
  selecting layer metadata, all assignments, summary and model-loaded record.
  Raw stdout digest/size and selected line numbers are in the audit; the full
  raw output remains in the operator's `.tmp/offload-evidence-20261009/` record.
- `*.callback.*`: exact upstream C++ message literals passed through the
  **shipped** `libllama-common.so.0.6.0`, not a Python imitation of its format.
  Layer/device callback inputs are synthetic. Full/partial/fallback callbacks
  establish logger/parser format compatibility, **not actual GPU placement**.
  verbosity3 suppresses library INFO and DEBUG; verbosity4 emits INFO only;
  verbosity5 also emits device assignments. Plain logs go to stderr, JSONL
  goes to stdout. `suppressed-v3.callback.txt` is deliberately empty.

The callback source is included. Message origins in the pinned source:

- `src/llama-model.cpp:2083`: model layer metadata, INFO.
- `src/llama-model.cpp:1596/1601`: actual assigned layer device, DEBUG.
- `src/llama-model.cpp:1926`: offload count, INFO (count derives from options;
  it cannot substitute for device assignments).
- `src/llama-model-loader.cpp:1404`: preferred tensor buffer fallback, DEBUG.
- `common/log.cpp:533/545`: INFO maps to threshold4, DEBUG to5.
- `common/log.cpp:93/105`: plain stderr versus JSONL stdout.

On an operator-owned CPU host, compile callback sources against the extracted
pinned engine package (Ubuntu24.04 ABI; libgomp is required):

```sh
g++ -std=c++17 callback.cpp -L "$ENGINE_ROOT" -lllama-common \
  -Wl,--allow-shlib-undefined -o callback
LD_LIBRARY_PATH="$ENGINE_ROOT" ./callback 3 0 29
LD_LIBRARY_PATH="$ENGINE_ROOT" ./callback 4 0 29
LD_LIBRARY_PATH="$ENGINE_ROOT" ./callback 5 0 29
LD_LIBRARY_PATH="$ENGINE_ROOT" ./callback 5 1 29
LD_LIBRARY_PATH="$ENGINE_ROOT" ./callback 5 1 28
```

`callback-fallback.cpp` uses the same logger and full29 synthetic assignments,
then emits the source-derived tensor fallback before the summary.
These commands perform no model inference or GPU operation. JSONL timestamps
are measured by the real logger and may differ between regenerations.

CPU-negative audit uses `--device none --gpu-layers 0 --fit off --no-host
--no-repack --ctx-size 512 --parallel 1 --threads 2 --no-warmup --no-webui
--log-verbosity 5 --log-jsonl`, a private Unix socket, no network/GPU access,
two CPUs, 3GiB memory and a 60-second deadline. Its model digest is in the audit.
It loads the real model; it does not generate text.

Tests replay raw fixture bytes in split chunks, reject summary-only and
partial/contradictory evidence, and retain extracted facts after log-tail
truncation. GPU execution, normal RunTTL/canonical ACK and nonempty Asset
history remain unverified until a separately authorized trial.
