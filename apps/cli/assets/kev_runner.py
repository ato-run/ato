"""Score one state with a locally installed Kev Model Set.

Ato owns the bytes: it downloads, pins and verifies the Model Set, then hands the
verified directory to this runner. The runner owns only the forward pass. It reads
one JSON request on stdin and writes one JSON response on stdout, so the CLI never
parses model output and the model never sees the filesystem layout of ATO_HOME.

Request:  {"state": <text|json>, "questions": {<id>: <question>}, "model_root": <path>}
Response: {"answers": {...}, "raw": [[...]], "runtime": {...}, "checkpoint": {...}}

The reference runtime resolves a LoRA checkpoint's base by Hub id. Ato already
holds those exact bytes, so the runner redirects that one id to the verified
directory and leaves every other resolution untouched. Nothing is downloaded.
"""
import json
import os
import sys
import time
from pathlib import Path


def fail(code, message):
    json.dump({"error": {"code": code, "message": message}}, sys.stdout)
    sys.stdout.write("\n")
    sys.exit(1)


def link(source, target):
    """Hard-link a verified payload into the run directory the runtime expects."""
    if target.exists():
        return
    try:
        os.link(source, target)
    except OSError:
        import shutil

        shutil.copyfile(source, target)


def main():
    request = json.load(sys.stdin)
    root = Path(request["model_root"]).resolve()
    base = root / "base"
    adapter = root / "adapter"
    tokenizer = root / "tokenizer"
    head = root / "head.pt"
    for required in (base, adapter, tokenizer, head):
        if not required.exists():
            fail("model_set_incomplete", f"{required} is missing; run: ato model install kev-0.8b")

    try:
        import kev.checkpoint as checkpoint
        import transformers
        from transformers import AutoTokenizer
    except ImportError as error:
        fail("runtime_unavailable", f"the Kev runtime is not importable: {error}")

    # The runtime reads a run directory: adapter files, head.pt and the tokenizer
    # at the top level. Build it from the verified payload without copying weights.
    run_dir = root / "run"
    run_dir.mkdir(parents=True, exist_ok=True)
    for source, name in (
        (adapter / "adapter_config.json", "adapter_config.json"),
        (adapter / "adapter_model.safetensors", "adapter_model.safetensors"),
        (head, "head.pt"),
        (tokenizer / "tokenizer.json", "tokenizer.json"),
        (tokenizer / "tokenizer_config.json", "tokenizer_config.json"),
    ):
        link(source, run_dir / name)

    resolve_run_original = checkpoint.resolve_run
    autoconfig_original = transformers.AutoConfig.from_pretrained

    def is_pinned_base(name):
        return "Qwen/Qwen3.5-0.8B-Base" in str(name)

    def resolve_run(name):
        return str(base) if is_pinned_base(name) else resolve_run_original(name)

    def autoconfig_from_pretrained(name, **kwargs):
        if is_pinned_base(name):
            kwargs.pop("revision", None)
            return autoconfig_original(str(base), **kwargs)
        return autoconfig_original(name, **kwargs)

    checkpoint.resolve_run = resolve_run
    checkpoint.load_tokenizer = lambda name, revision=None: AutoTokenizer.from_pretrained(str(base))
    transformers.AutoConfig.from_pretrained = autoconfig_from_pretrained

    from kev.api import SystemOneRequest, to_answers, to_record
    from kev.device import default_device
    from kev.model import admit

    started = time.time()
    try:
        ckpt = checkpoint.Checkpoint(str(run_dir))
        device = default_device()
        tokenizer_obj, model = ckpt.load(device, checkpoint.LoadOptions(backend="mlx"))
    except Exception as error:  # the runtime raises its own types; report them verbatim
        fail("model_load_failed", f"{type(error).__name__}: {error}")
    load_seconds = time.time() - started

    try:
        req = SystemOneRequest(state=request["state"], questions=request["questions"])
        record, meta = to_record(req)
        encoded = admit(model, tokenizer_obj, record)
        started = time.time()
        probs = model.probs(encoded)
        infer_ms = (time.time() - started) * 1000
    except Exception as error:
        fail("inference_failed", f"{type(error).__name__}: {error}")

    import mlx.core
    import mlx_lm

    json.dump(
        {
            "answers": to_answers(probs, meta),
            "raw": [[float(p) for p in row] for row in probs],
            "checkpoint": {
                "weights": ckpt.meta.weights,
                "base": ckpt.meta.base,
                "base_revision": ckpt.meta.base_revision,
                "head_dim": ckpt.meta.head_dim,
                "temperature": float(getattr(model.head, "temperature", 0.0)),
            },
            "runtime": {
                "backend": "mlx",
                "device": str(device),
                "mlx": getattr(mlx.core, "__version__", None),
                "mlx_lm": getattr(mlx_lm, "__version__", None),
                "python": sys.version.split()[0],
                "load_seconds": round(load_seconds, 3),
                "inference_ms": round(infer_ms, 1),
            },
        },
        sys.stdout,
    )
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
