"""Answer Ato's System One generation request with a local MLX model.

The generation contract is a choice task: the caller asks which operation to
draft (python_script or decline) and which opaque entrypoint to use, and carries
its own instructions in the request. This runner uses those instructions verbatim
so the prompt cannot drift from the contract, and maps the model's answer back
into the System One response shape.

One request per line on stdin, one response per line on stdout, model loaded once.
"""
import json
import sys
import time

import mlx.core as mx
from mlx_lm import generate, load

MODEL_PATH = sys.argv[1]
model, tokenizer = load(MODEL_PATH)


def failure(code, message):
    return {"error": {"code": code, "message": message}}


def prompt_for(request):
    state = request.get("state") or {}
    questions = request.get("questions") or {}
    instructions = ""
    for question in questions.values():
        if isinstance(question, dict) and question.get("instructions"):
            instructions = question["instructions"]
            break
    entrypoints = state.get("entrypoint_ids") or []
    failures = state.get("failures") or []
    lines = [
        instructions,
        "Available opaque entrypoints: " + (", ".join(entrypoints) if entrypoints else "none") + ".",
        "Prior failures: " + (json.dumps(failures) if failures else "none") + ".",
        "Answer with JSON only, no prose:",
        '{"operation": "<python_script|decline>", "entrypoint": "<an entrypoint id or none>"}',
    ]
    return "\n".join(line for line in lines if line)


def choice(selected, keys, confidence=0.9):
    others = [key for key in keys if key != selected]
    share = (1.0 - confidence) / len(others) if others else 0.0
    return {
        "type": "choice",
        "choice": selected,
        "confidence": confidence,
        "probabilities": {key: (confidence if key == selected else share) for key in keys},
    }


def answer(request):
    state = request.get("state") or {}
    entrypoints = state.get("entrypoint_ids") or []
    prompt = prompt_for(request)
    messages = [{"role": "user", "content": prompt}]
    text = tokenizer.apply_chat_template(messages, add_generation_prompt=True)
    started = time.time()
    out = generate(model, tokenizer, prompt=text, max_tokens=64, verbose=False)
    elapsed_ms = (time.time() - started) * 1000
    parsed = {}
    try:
        parsed = json.loads(out[out.index("{"): out.rindex("}") + 1])
    except (ValueError, json.JSONDecodeError):
        parsed = {}
    operation = parsed.get("operation")
    if operation not in ("python_script", "decline"):
        operation = "decline"
    entrypoint = parsed.get("entrypoint")
    if entrypoint not in entrypoints:
        entrypoint = "none"
    return {
        "model": request.get("model", "local"),
        "answers": {
            "operation": choice(operation, ["python_script", "decline"]),
            "entrypoint": choice(entrypoint, entrypoints + ["none"]),
        },
        "usage": {"input_tokens": len(text), "output_tokens": len(out)},
    }


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except ValueError as error:
            response = failure("invalid", f"the request is not valid JSON: {error}")
        else:
            try:
                response = answer(request)
            except Exception as error:  # report, keep serving
                response = failure("inference_failed", f"{type(error).__name__}: {error}")
        json.dump(response, sys.stdout)
        sys.stdout.write("\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
