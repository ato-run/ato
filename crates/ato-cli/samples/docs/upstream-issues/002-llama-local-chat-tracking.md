# [tracking] llama-local-chat sample pending GPU broker v1 API

<!--
LABELS: tracking, gpu-broker
REPO: ato-run/ato-cli
-->

## Summary

The `samples/00-quickstart/llama-local-chat` sample (demonstrating the blog post's core claim of "click → 5 min → local LLM") cannot be written until the GPU broker API is stable at v1.

This issue tracks the dependency so that sample work can resume once the blocker ships.

## Blocker

The sample requires:
1. `[gpu]` section in `capsule.toml` with `backend = "metal" | "cuda" | "cpu"`  
2. `content_addressed_inputs` for HuggingFace model blobs (progressive download UX)  
3. GPU broker API at runtime (`ato_gpu::acquire_context()` or equivalent)  
4. `progressive_launch = true` to show download progress before first request

None of these are stable in v0.4.x. The GPU broker is underspecified.

## What the sample will look like

```toml
# capsule.toml (target)
schema_version = "0.3"
name = "llama-local-chat"
version = "0.1.0"
type = "app"
runtime = "source/python"
runtime_version = "3.11.10"
run = "python server.py"
port = 8080

[gpu]
backend = "metal"   # or "cuda" / "cpu" fallback
budget_mb = 8192

[content_addressed_inputs]
model = "hf://meta-llama/Llama-3.1-8B-Instruct-Q4_K_M.gguf"
```

Expected UX: missing model → progressive download → browser opens with chat UI.

## Definition of done (for unblocking this sample)

- [ ] `[gpu]` section is parsed and exposed in `ExecutionPlan`
- [ ] `content_addressed_inputs` supports `hf://` URIs with progressive download
- [ ] GPU broker API has a stable Rust interface (no breaking changes planned for v0.5)
- [ ] Sample can reach E302 (consent gate) on a Mac with Apple Silicon

## Related

- Blog post §3 (core UX claim: "5 min to local LLM")  
- `samples/00-quickstart/` — home tier for this sample once unblocked
