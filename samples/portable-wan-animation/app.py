"""One fixed Animation workflow, with dependencies delivered as immutable inputs."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

WORKSPACE = Path(__file__).resolve().parent
SCRATCH = Path(os.environ["TMPDIR"]) / "wan"
SOFTWARE = Path(os.environ["ATO_INPUT_PATH_SOFTWARE"])
MODELS = Path(os.environ["ATO_INPUT_PATH_WAN_MODELS"])
ASSETS = Path(os.environ.get("ATO_INPUT_ASSETS_DIR", SCRATCH / "no-assets"))
OUTPUT = Path(os.environ["ATO_OUTPUT_DIR"])
SCRATCH.mkdir(parents=True, exist_ok=True)
STATE = {"phase": "installing", "disk_after_delivery": shutil.disk_usage(SCRATCH)._asdict()}
LOCK = threading.Lock()
CHILD = None


def update(**values):
    with LOCK:
        STATE.update(values)


def checksum(path):
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def asset_report():
    return [{"asset_id": path.parent.name, "filename": path.name,
             "bytes": path.stat().st_size, "sha256": checksum(path)}
            for path in sorted(ASSETS.glob("*/*")) if path.is_file()]


def status():
    with LOCK:
        result = dict(STATE)
    worker_status = SCRATCH / "worker-status.json"
    if worker_status.exists():
        result.update(json.loads(worker_status.read_text()))
    if CHILD and CHILD.poll() is not None and result.get("phase") != "generated":
        result.update(phase="failed", error=f"ComfyUI worker exited {CHILD.returncode}")
    return result


def prepare():
    global CHILD
    started = time.monotonic()
    try:
        sources = SCRATCH / "sources"
        sources.mkdir()
        for archive in sorted((SOFTWARE / "sources").glob("*.tgz")):
            dest = sources / archive.stem
            dest.mkdir()
            with tarfile.open(archive) as package:
                package.extractall(dest, filter="data")
            extracted = list(dest.iterdir())
            if len(extracted) != 1 or not extracted[0].is_dir():
                raise RuntimeError(f"Unexpected source archive layout: {archive.name}")
            extracted[0].rename(sources / (archive.stem + "-tree"))
        comfy = sources / "ComfyUI-tree"
        for custom in sources.glob("ComfyUI-*-tree"):
            (comfy / "custom_nodes" / custom.name.removesuffix("-tree")).symlink_to(custom, target_is_directory=True)
        shutil.rmtree(comfy / "models")
        (comfy / "models").symlink_to(MODELS, target_is_directory=True)
        for name, target in [("image.jpeg", "animate_image.jpeg"), ("video.mp4", "animate_video.mp4")]:
            matches = list(ASSETS.glob("*/" + name))
            if len(matches) != 1:
                raise RuntimeError(f"Expected one input Asset named {name}; got {len(matches)}")
            shutil.copyfile(matches[0], comfy / "input" / target)
        environment = dict(os.environ, PIP_DISABLE_PIP_VERSION_CHECK="1", PIP_NO_CACHE_DIR="1",
                           PYTHONDONTWRITEBYTECODE="1", HF_HUB_OFFLINE="1", TRANSFORMERS_OFFLINE="1")
        log = (SCRATCH / "software-install.log").open("w")
        venv = SCRATCH / "venv"
        subprocess.run([sys.executable, "-m", "venv", "--copies", str(venv)], env=environment, stdout=log, stderr=log, check=True)
        python = venv / "bin/python"
        subprocess.run([str(python), "-m", "pip", "install", "--no-index", "--only-binary=:all:",
                        "--require-hashes", "--find-links", str(SOFTWARE / "wheels"),
                        "-r", str(SOFTWARE / "requirements.lock")], env=environment, stdout=log, stderr=log, check=True)
        log.close()
        update(assets=asset_report(), setup_seconds=round(time.monotonic() - started, 3), phase="loading")
        environment.update(WAN_SCRATCH=str(SCRATCH), WAN_COMFY=str(comfy), WAN_OUTPUT=str(OUTPUT), WAN_WORKSPACE=str(WORKSPACE))
        CHILD = subprocess.Popen([str(python), "-B", str(WORKSPACE / "worker.py")], cwd=comfy,
                                 env=environment, stdout=(SCRATCH / "comfy.log").open("w"), stderr=subprocess.STDOUT)
    except Exception as error:
        update(phase="failed", error=str(error))


HTML = b'''<!doctype html><meta charset="utf-8"><title>Wan Animation</title>
<button id="generate" disabled>Generate animation</button><p id="phase">Preparing</p><video id="video" controls></video>
<script>
const button=document.getElementById('generate'), phase=document.getElementById('phase'), video=document.getElementById('video');
button.onclick=async()=>{button.disabled=true;await fetch('/generate',{method:'POST'});};
setInterval(async()=>{const s=await(await fetch('/status')).json();button.disabled=s.phase!=='ready';
phase.textContent=s.error||({installing:'Preparing',loading:'Loading',ready:'Ready',generating:'Generating',generated:'Animation ready'}[s.phase]||s.phase);
if(s.phase==='generated'&&!video.src)video.src='/generated.mp4';},2000);
</script>'''


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/health":
            body, content_type = b"wan-animation-ready\n", "text/plain"
        elif self.path == "/":
            body, content_type = HTML, "text/html; charset=utf-8"
        elif self.path == "/status":
            body, content_type = json.dumps(status()).encode(), "application/json"
        elif self.path == "/diagnostics":
            body = json.dumps({name: (SCRATCH / name).read_text(errors="replace")[-10000:]
                               for name in ["software-install.log", "comfy.log"]
                               if (SCRATCH / name).exists()}).encode()
            content_type = "application/json"
        elif self.path == "/generated.mp4" and (OUTPUT / "animation.mp4").exists():
            body, content_type = (OUTPUT / "animation.mp4").read_bytes(), "video/mp4"
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        if self.path != "/generate":
            self.send_error(404)
            return
        if status().get("phase") != "ready":
            self.send_error(409, "Animation worker is not ready")
            return
        with LOCK:
            request = SCRATCH / "generate.request"
            if request.exists():
                self.send_error(409, "Generation already requested")
                return
            request.write_text("animation\n")
        self.send_response(202)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, *_):
        pass


server = ThreadingHTTPServer((os.environ.get("APP_LISTEN_HOST", "127.0.0.1"), int(os.environ.get("ATO_ENDPOINT_APP_HTTP_PORT", "8000"))), Handler)
threading.Thread(target=prepare, daemon=True).start()
server.serve_forever()
