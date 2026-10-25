# Fixed GPU chat sample

Status: draft implementation contract, 2026-10-08. TODO #141 under #136.

`samples/portable-gpu-llm` declares one Python process D and two immutable
Model Set inputs: Qwen2.5-1.5B-Instruct Q4_K_M and the pinned llama.cpp b11429
CUDA 12.8 engine/runtime archives. Their SHA-256 and sizes were read from the
official upstream metadata; both software archives were fetched and hashed
locally. Weights remain external objects. Source/provider metadata never changes
the canonical Model Set references or K based on actual execution.

Placement, host measurement, allowed devices and paid lifecycle remain the
Runner/ProviderAdapter's authority. The process never receives a RunPod key,
Docker socket or provisioning permission. It uses the current Data Grant input
mounts and output directory. The Run needs a Linux x86_64 NVIDIA host, pinned
Python 3.11.11, at least 4 GiB VRAM, 16 GiB usable memory and 8 GiB free scratch.
Local CLI host-conditioned admission remains refused; no alternate CPU D is
inferred. This sample does not implement local process/OCI GPU assignment.

The engine listens on a Run-private Unix socket. Only the declared HTTP Port is
exposed. Offline mode, explicit CUDA0, all GPU layers and disabled automatic
fit are fixed process arguments. Readiness additionally requires the pinned
engine's complete layer-offload log evidence and an actual health response.
Missing device/load evidence yields a non-ready route, not a CPU result. The
frozen K asks for this readiness body; actual generation is a separate functional
acceptance and must not be inferred from `fully_satisfied` alone.

Generation uses one bounded streaming request at a time. A persisted request
ID binds its exact prompt; duplicates return that job and changed input conflicts.
The UI retains the ID after a lost response, so Retry send does not create a new
generation. Cancellation shuts down the Unix connection, including HTTP/1.0
responses that own their socket independently. The partial response is retained.
Cancelling a generation does not stop the loaded model or confirm Pod deletion.

History is bounded to 64 jobs and one MiB of JSON. Checkpoints are atomic, and
the first checkpoint must succeed before starting inference. They live in the
Instance output path, separated from read-only weights. The existing save path
uploads the checkpoint after confirmed Run stop. Exactly one owner-granted
`chat-history.json` Asset can restore the next Run; incomplete jobs become
interrupted and never resume inference automatically. Automatic selection of the
saved Asset for subsequent Runs remains a separate Coordinator/UI requirement.

Fixture coverage includes stream completion, cancellation disconnect, duplicate
requests, conflicting input, saved restart, interrupted restart, bounds, first
checkpoint failure and readiness refusal. Fixture inference is not real CUDA
execution. Paid RunPod launch, GPU/driver compatibility, nonempty generation,
save/restore across actual Runs and owned-resource deletion require separately
authorized real-host acceptance before publishing this sample as working.
