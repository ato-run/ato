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
declared ready body only after engine HTTP200 and pinned JSONL evidence confirm
28 repeating layers plus the output layer assigned to CUDA0, matching model
metadata and 29/29 summary, without tensor-buffer fallback/override. Input
embedding remains on CPU as designed by b11429. Unknown/missing CUDA or partial offload fails closed. There
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

## 2026-10-09: 準備・ABI・診断

初回RunPod試験では固定Contractの`/health`が503となり、LLM起動は未達だった。
空の履歴保存と停止・Pod削除は確認したが、engine logがscratchにだけ存在し、
CUDA認識・モデル読込の実測結果は残らなかった。

現在のDは`derivation.startup`で`/health`・180秒を明示する。共通runtimeは
process開始、HTTP応答可能、準備完了を区別し、準備完了後に固定Kを観測する。
Kのstatus 200とbody digestは維持し、不成立を成功まで繰り返さない。
180秒はengine起動後だけでなく、sampleの展開・loader/CUDA照会・モデル準備全体に適用する。

b11429の配布物はGLIBC 2.38 / GLIBCXX 3.4.32を必要とする。
DのABI下限、APIのimmutable image profile照合、Runnerの実環境検査、
sampleのlibrary/`--help` loader検査で起動前に拒否できる。
RunPodで明示選択する対応imageは
`runpod/pytorch@sha256:0a360022e8de4375af99430f84e8b38951acc397252163a37ceac7204d01be35`。
元のUbuntu22.04 digestでは新sampleのcreateを拒否する。自動image切替はない。
旧試験bundleはimmutableでこの新要件を持たないため再利用せず、新しいbundleをpack・検証する。
新Runnerの`runtime_feature=process_startup_v1`も必須であり、旧Runner配備のまま動作済みとは扱わない。

停止後の出力は`chat-history.json`、`startup-diagnostics.json`と存在する
`engine-stdout.log` / `engine-stderr.log`。ログは各256KiBの末尾に制限し、
診断はCUDA照会結果、loader結果、準備フェーズ、HTTP状態、全layer offload数、
engine終了コード、capture完了/失敗を記録する。管理キーやenv全体は保存しない。
共通Runnerの`runner-process-startup.json` / `runner-process.log`と、起動失敗時の
`runner-runtime-start-failure.json`も同じAsset保存経路へ渡す（最大7ファイル、grant上限8）。
保存は停止確認後、terminal failure報告前に行う。保存できなかった出力は保存済みとしない。

```sh
mkdir -p .tmp/gpu-llm-tests
TMPDIR="$PWD/.tmp/gpu-llm-tests" python3 -B -m unittest discover -s samples/portable-gpu-llm/tests -v
```

Unix socket pathが100byteを超える長いworktreeでは、workspaceの短い`.tmp/`配下に
専用TMPDIRを作る。`test_startup.py`はCPU fixture processと実HTTP/Unix socketで
遅延503→200、異常終了、準備timeout、取消、ABI拒否、bounded logを確認する。
fixtureのCUDA表示・GPU配置入力は模擬であり、実CUDA検証ではない。
生成文の実Asset保存、時間制限による自動停止、保存猶予切れは引き続き実GPU未検証。
追加のPod作成は別途承認が必要。

## 2026-10-09: b11429の配置証拠

2回目はengine HTTP200に到達したが、回収stderr 1164 bytesに全offloadの証拠がなく失敗した。
b11429はlibrary INFOをverbosity 4としてfilterするため、既定3では集計が出ない。
固定版の配布loggerでも同じ抑制を再現した。現在は`--log-verbosity 5 --log-jsonl`で
JSONLをstdoutへ出し、層別device割り当て・model層数・集計を照合する。
`--no-host --no-repack`でCPU input bufferの期待される変更を避け、
tensor buffer fallbackやoverrideが出た場合も準備成功を拒否する。
`/props`には層配置情報がない。HTTP200、CUDA認識、argv、メモリ量だけではreadyにしない。

`offload_evidence.py`はraw logの末尾を切る前にboundedな配置証拠を抽出し、
診断の`offload_evidence`へ残す。未証明のまま期限に達した場合、
`error=startup_timeout`と`failure_reason=offload_evidence_missing`を併記する。
engine未起動、HTTP準備中、部分配置、証拠矛盾は別の理由として記録する。

[fixtureの出所](../../tests/fixtures/portable-gpu-llm/b11429/README.md)には、前回の実ログ、固定ソースの形式を
配布libraryで出力したcallback、実QwenモデルのCPU配置ログ、SHA-256を記録した。
正規表現だけに合わせた模擬ログを互換性の根拠にはしない。
実GPUで29/29配置と生成を確認する検証、通常Run期限のcanonical停止ACK、
生成履歴のAsset保存は依然未検証。3回目のPod作成は未承認。
