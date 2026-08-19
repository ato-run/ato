# ComfyUI Generic Browser Continuation Experiment

Status: Partial — UI bootstrap only

## Goal

This experiment measures the boundary of generic Browser continuation against
vanilla ComfyUI. It uses only `ato.browser@1`, `ato.replay@1`, and the ordinary
Capsule lifecycle. It does not introduce a ComfyUI Adapter, Materializer,
Capsule type, Replay branch, or Core semantic primitive.

The question is deliberately narrow: given an independently materialized
initial ComfyUI state, can a fresh Chrome receive recorded physical UI input,
reach the same independently checked node-graph state, and create a continued
future?

## Pinned setup

- Runner: `ubuntu-sugamo`
- Chrome: `150.0.7871.186`
- Python: `3.14.4`
- ComfyUI: vanilla upstream commit
  `3aba3daef37af4692c86cf5b2122b488ab941325`
- Custom nodes: none
- Models and generated-image workflows: none
- First workload: graph manipulation only; no text prompt is recorded

The staging host had no GPU. The experiment therefore uses CPU-only PyTorch
dependencies and does not treat image generation as a prerequisite for the
Browser-boundary test.

## Planned evidence

1. Start ComfyUI with a known initial workflow already present.
2. Record a node drag (`pointerdown`, `pointermove*`, `pointerup`) through the
   generic Browser Host.
3. Seal and encap with `ato.replay@1`.
4. Start a fresh ComfyUI process and fresh Chrome profile at the same initial
   workflow.
5. Replay and inspect node ID/position through DOM or the ComfyUI API from the
   test harness only.
6. Perform a second real drag, observe a new ComputationRef, and re-encap the
   continued branch.

Parameter controls are limited to non-text widgets. Prompt text is a negative
privacy check: it must not appear in Browser Record payloads or bundles and is
not expected to replay in v1.

## Failure taxonomy

Every failure is classified before proposing any implementation:

| Class | Layer | Example follow-up |
| --- | --- | --- |
| A | Browser interaction representation | viewport/surface transform |
| B | Browser environment/materialization | fresh profile or display setup |
| C | Application persistent state | initial workflow materialization |
| D | Runtime/dependency | Python, CPU/GPU, models, custom nodes |
| E | Network/side-effect causality | queue or WebSocket behavior |
| F | Verification/Contract | independent graph-state assertion |
| G | Application-specific semantics | only then evaluate a future Protocol |

## Current evidence

The staging host initially lacked `python3.14-venv`, which was installed as an
isolated runtime prerequisite. A default dependency resolution then selected
CUDA wheels despite the CPU-only host; that installation was stopped before
completion and replaced with the PyTorch CPU index. This is a Class D runtime
issue, not evidence for a ComfyUI Adapter.

Vanilla ComfyUI then started successfully at `http://127.0.0.1:8188` with
`main.py --cpu --listen 127.0.0.1 --port 8188` and returned HTTP `200`. A
normal start without `--cpu` tried to select a CUDA device and failed. This
confirms that the real-world application can run on the staging host for a
graph-only experiment, but it does **not** yet establish Level B/C replay.

The remaining blocking condition is a Class C initial-materialization contract:
the current ComfyUI frontend starts with application-managed graph/settings
state, while this branch has no declared, portable way to place a known
workflow into each fresh ComfyUI realization. Injecting that graph through the
Browser Adapter would violate the experiment; placing it through storage or a
private API without a materialization contract would not prove continuation.
No ComfyUI-specific Adapter is justified by this observation. The next
experiment must first declare the initial workflow as materialized application
state, then repeat the node-drag sequence above.

Public staging delivery remains separate: Browser Host host-local acceptance
is green, while PWA Pixel Stream/RFB provisioning for a host-owned Chrome is
not yet wired. No result in this document claims general ComfyUI Capsule or
image-generation support.
