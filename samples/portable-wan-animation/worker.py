"""Drive the pinned ComfyUI queue without opening a second network Port."""
import asyncio
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import threading
import time
import uuid

SCRATCH = Path(os.environ["WAN_SCRATCH"])
COMFY = Path(os.environ["WAN_COMFY"])
OUTPUT = Path(os.environ["WAN_OUTPUT"])
WORKSPACE = Path(os.environ["WAN_WORKSPACE"])


def publish(**state):
    temporary = SCRATCH / "worker-status.next"
    temporary.write_text(json.dumps(state))
    temporary.replace(SCRATCH / "worker-status.json")


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


try:
    sys.path.insert(0, str(COMFY))
    sys.argv = ["main.py", "--disable-auto-launch", "--disable-partner-nodes"]
    import main
    import nodes
    import execution
    from workflow import compile_prompt
    loop, server, _start_http = main.start_comfyui()
    threading.Thread(target=loop.run_forever, daemon=True).start()
    workflow = json.loads((WORKSPACE / "workflow.json").read_text())
    object_info = {}
    for node in workflow["nodes"]:
        cls = nodes.NODE_CLASS_MAPPINGS.get(node["type"])
        if cls:
            object_info[node["type"]] = {"input": cls.INPUT_TYPES()}
    prompt = compile_prompt(workflow, object_info)
    prompt_id = str(uuid.uuid4())
    validation = asyncio.run(execution.validate_prompt(prompt_id, prompt, None))
    if not validation[0]:
        raise RuntimeError("Pinned workflow validation failed: " + json.dumps(validation[1:]))
    publish(phase="ready", python=sys.version.split()[0], frame_load_cap=81, workflow_sha256=digest(WORKSPACE / "workflow.json"))
    while not (SCRATCH / "generate.request").exists():
        time.sleep(0.5)
    started = time.monotonic()
    publish(phase="generating", prompt_id=prompt_id)
    # Same queue and PromptExecutor used by the pinned /prompt HTTP handler.
    server.prompt_queue.put((0, prompt_id, prompt, {}, validation[2], {}))
    samples = []
    peak_used = 0
    while True:
        disk = shutil.disk_usage(SCRATCH)
        peak_used = max(peak_used, disk.used)
        gpu = subprocess.run(["nvidia-smi", "--query-gpu=memory.used", "--format=csv,noheader,nounits"], capture_output=True, text=True)
        rss = next((int(line.split()[1]) // 1024 for line in Path("/proc/self/status").read_text().splitlines() if line.startswith("VmRSS:")), 0)
        if gpu.returncode == 0:
            samples.append((int(gpu.stdout.strip().splitlines()[0]), rss))
        history = server.prompt_queue.get_history(prompt_id)
        if prompt_id in history:
            break
        time.sleep(1)
    result = history[prompt_id]
    if not result["status"]["completed"] or result["status"]["status_str"] != "success":
        raise RuntimeError("Generation failed: " + json.dumps(result["status"]))
    videos = []
    for node_output in [result.get("outputs", {}).get("30", {})]:
        for video in node_output.get("gifs", []):
            path = Path(video.get("fullpath") or (COMFY / "output" / video.get("subfolder", "") / video["filename"]))
            if path.suffix == ".mp4" and path.is_file():
                videos.append(path)
    if not videos:
        raise RuntimeError("Generation succeeded but produced no MP4")
    # Node 30 is the pinned workflow's final Wan video output.
    video = videos[-1]
    OUTPUT.mkdir(exist_ok=True)
    shutil.copyfile(video, OUTPUT / "animation.mp4")
    import av
    with av.open(str(OUTPUT / "animation.mp4")) as container:
        decoded = list(container.decode(video=0))
        media = {"decoded_frames": len(decoded), "width": decoded[0].width, "height": decoded[0].height,
                 "frame_rate": str(container.streams.video[0].average_rate)}
    # 81 is the loader's cap, not a promise that the source has 81 frames
    # after resampling. Phase 0's official Animation MP4 decodes to 56 frames.
    if not 0 < media["decoded_frames"] <= 81:
        raise RuntimeError(f"Decoded frame count outside the fixed loader cap: {media['decoded_frames']}")
    evidence = {"generation": "succeeded", "generation_seconds": round(time.monotonic() - started, 3),
                "prompt_id": prompt_id, "mode": "Animation", "frame_load_cap": 81, "media": media,
                "output_bytes": (OUTPUT / "animation.mp4").stat().st_size,
                "output_sha256": digest(OUTPUT / "animation.mp4"),
                "workflow_sha256": digest(WORKSPACE / "workflow.json"),
                "vram_peak_mib": max((s[0] for s in samples), default=0),
                "rss_peak_mib": max((s[1] for s in samples), default=0),
                "generation_peak_disk_used": peak_used, "disk_after_generation": shutil.disk_usage(SCRATCH)._asdict()}
    (OUTPUT / "generation.json").write_text(json.dumps(evidence, sort_keys=True))
    publish(phase="generated", **evidence)
    while True:
        time.sleep(1)
except Exception as error:
    publish(phase="failed", error=str(error))
    raise
