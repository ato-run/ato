"""OpenClaw + Ollama bootstrap: zero-config setup with multi-platform support."""

import argparse
import json
import os
import platform
import pwd
import shutil
import subprocess
import sys
import time
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

# ─── Constants ────────────────────────────────────────────────────────────────

OLLAMA_HOST = os.environ.get("OLLAMA_HOST", "http://127.0.0.1:11434")
GATEWAY_TOKEN = "local-dev"
GATEWAY_PORT = "18789"
CAPSULE_ID = "openclaw-local-llm"

# nvm installer URL (used when node/npm not available on the system)
NVM_INSTALL_URL = "https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.3/install.sh"

# ato isolates HOME → resolve the real HOME for persistent storage
REAL_HOME = Path(pwd.getpwuid(os.getuid()).pw_dir)
CAPSULE_DIR = Path(__file__).parent

# Persistent state (survives across ato runs; stored in real HOME)
STATE_FILE = REAL_HOME / ".openclaw" / f".{CAPSULE_ID}-state.json"

# OpenClaw binary installed via local package.json (version-pinned)
OPENCLAW_BIN = CAPSULE_DIR / "node_modules" / ".bin" / "openclaw"

# package.json content written dynamically so ato doesn't auto-detect an
# npm ci install step (which would fail before node is provisioned)
_OPENCLAW_PACKAGE_JSON = {
    "name": "openclaw-local-llm",
    "version": "0.1.0",
    "private": True,
    "description": "OpenClaw CLI for openclaw-local-llm capsule",
    "devDependencies": {
        "openclaw": "latest",
    },
}

MODEL_CATALOG = [
    {
        "id": "qwen3:8b",
        "label": "Qwen3 8B   [~5GB]  ★ recommended — 8GB RAM+",
        "context_window": 128000,
        "default": True,
    },
    {
        "id": "qwen3:14b",
        "label": "Qwen3 14B  [~9GB]  — higher accuracy",
        "context_window": 128000,
    },
    {
        "id": "qwen3:32b",
        "label": "Qwen3 32B  [~20GB] — best quality (16GB+ RAM)",
        "context_window": 131072,
    },
    {
        "id": "llama3.2:3b",
        "label": "Llama 3.2 3B [~2GB] — lightweight / low RAM",
        "context_window": 128000,
    },
    {
        "id": "deepseek-r1:8b",
        "label": "DeepSeek R1 8B [~5GB] — reasoning-focused",
        "context_window": 128000,
    },
    {"id": "__custom__", "label": "Enter custom model id..."},
]

DEFAULT_MODEL = next(m["id"] for m in MODEL_CATALOG if m.get("default"))

# ─── Platform Detection ───────────────────────────────────────────────────────


def detect_platform() -> str:
    """Return 'macos' | 'wsl2' | 'ubuntu' | 'linux' | 'unsupported'."""
    if platform.system() == "Darwin":
        return "macos"
    if platform.system() == "Linux":
        try:
            version = Path("/proc/version").read_text().lower()
            if "microsoft" in version or "wsl2" in version:
                return "wsl2"
        except OSError:
            pass
        try:
            lsb = Path("/etc/lsb-release").read_text()
            if "Ubuntu" in lsb:
                return "ubuntu"
        except OSError:
            pass
        return "linux"
    return "unsupported"


def wsl_has_systemd() -> bool:
    try:
        r = subprocess.run(["systemctl", "--version"], capture_output=True, timeout=3)
        return r.returncode == 0
    except (FileNotFoundError, TimeoutError, subprocess.SubprocessError):
        return False


# ─── State Management ─────────────────────────────────────────────────────────


def load_state() -> dict:
    STATE_FILE.parent.mkdir(parents=True, exist_ok=True)
    if STATE_FILE.exists():
        try:
            return json.loads(STATE_FILE.read_text())
        except (json.JSONDecodeError, OSError):
            pass
    return {"schema": 1, "saved_model": None, "installed": {}}


def save_state(state: dict) -> None:
    STATE_FILE.parent.mkdir(parents=True, exist_ok=True)
    STATE_FILE.write_text(json.dumps(state, indent=2))


def state_track(state: dict, key: str, **attrs) -> None:
    state["installed"][key] = {
        "installed_by_us": True,
        "timestamp": datetime.now(timezone.utc).isoformat(),
        **attrs,
    }
    save_state(state)


def state_has(state: dict, key: str) -> bool:
    return state["installed"].get(key, {}).get("installed_by_us", False)


# ─── Ollama ───────────────────────────────────────────────────────────────────


def ollama_ready() -> bool:
    try:
        urllib.request.urlopen(f"{OLLAMA_HOST}/api/tags", timeout=3)
        return True
    except Exception:
        return False


def ensure_ollama_installed(plat: str, state: dict) -> None:
    if shutil.which("ollama"):
        return
    print("Installing Ollama...")
    if plat == "macos" and shutil.which("brew"):
        subprocess.run(["brew", "install", "ollama"], check=True)
        state_track(state, "ollama_bin", platform=plat, method="brew")
    elif plat in ("macos", "wsl2", "ubuntu", "linux"):
        subprocess.run(
            ["sh", "-c", "curl -fsSL https://ollama.com/install.sh | sh"],
            check=True,
        )
        state_track(state, "ollama_bin", platform=plat, method="curl_installer")
    else:
        sys.exit("ERROR: Unsupported platform. Install Ollama manually: https://ollama.com")


def start_ollama(state: dict) -> None:
    if ollama_ready():
        return
    print("Starting Ollama...")
    env = os.environ.copy()
    env["HOME"] = str(REAL_HOME)
    subprocess.Popen(
        ["ollama", "serve"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        env=env,
    )
    for _ in range(30):
        if ollama_ready():
            print("Ollama is ready.")
            return
        time.sleep(1)
    sys.exit("ERROR: Ollama failed to start within 30s")


def pull_model(model_id: str, state: dict, *, track: bool = True) -> None:
    """Pull a model via Ollama HTTP API; skip if already present locally."""
    try:
        resp = urllib.request.urlopen(f"{OLLAMA_HOST}/api/tags", timeout=5)
        data = json.loads(resp.read())
        names = [m.get("name", "") for m in data.get("models", [])]
        if any(n == model_id or n.startswith(f"{model_id}:") for n in names):
            print(f"Model {model_id} already available.")
            if track:
                state_track(state, f"ollama_model:{model_id}")
            return
    except Exception:
        pass

    print(f"Pulling model: {model_id} (this may take a while)...")
    req = urllib.request.Request(
        f"{OLLAMA_HOST}/api/pull",
        data=json.dumps({"name": model_id, "stream": True}).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=3600) as resp:
        for line in resp:
            try:
                status = json.loads(line)
                if "status" in status:
                    print(f"\r  {status['status']}", end="", flush=True)
            except json.JSONDecodeError:
                pass
    print()
    if track:
        state_track(state, f"ollama_model:{model_id}")


def list_local_ollama_models() -> list[str]:
    try:
        resp = urllib.request.urlopen(f"{OLLAMA_HOST}/api/tags", timeout=5)
        data = json.loads(resp.read())
        return [m.get("name", "") for m in data.get("models", [])]
    except Exception:
        return []


# ─── Node.js (via nvm) ────────────────────────────────────────────────────────


def _nvm_dir() -> Path:
    return Path(os.environ.get("NVM_DIR", str(REAL_HOME / ".nvm")))


def _find_nvm_node_bin() -> "Path | None":
    """Return the bin/ dir of the nvm-installed node, if present."""
    versions_dir = _nvm_dir() / "versions" / "node"
    if not versions_dir.exists():
        return None
    candidates = sorted(versions_dir.iterdir(), key=lambda p: p.name, reverse=True)
    for v in candidates:
        node_bin = v / "bin"
        if (node_bin / "node").exists():
            return node_bin
    return None


def ensure_node_installed(state: dict) -> None:
    """Install Node.js via nvm when node/npm are not available on the system.

    ato only provisions tools for the capsule's declared driver (Python here).
    Since OpenClaw is a Node.js application, this capsule self-bootstraps node
    the same way it self-bootstraps Ollama.
    """
    if shutil.which("node") and shutil.which("npm"):
        return

    # nvm may already be installed but not sourced in the current PATH
    existing_bin = _find_nvm_node_bin()
    if existing_bin:
        os.environ["PATH"] = f"{existing_bin}:{os.environ['PATH']}"
        if shutil.which("node") and shutil.which("npm"):
            return

    print("Installing Node.js via nvm...")
    nvm_dir = _nvm_dir()
    nvm_sh = nvm_dir / "nvm.sh"
    env = {**os.environ, "HOME": str(REAL_HOME), "NVM_DIR": str(nvm_dir)}

    if not nvm_sh.exists():
        subprocess.run(
            ["sh", "-c", f"curl -fsSL {NVM_INSTALL_URL} | PROFILE=/dev/null bash"],
            check=True,
            env=env,
        )

    subprocess.run(
        ["bash", "-c", f"source {nvm_sh} && nvm install --lts && nvm alias default lts"],
        check=True,
        env=env,
    )

    node_bin = _find_nvm_node_bin()
    if node_bin:
        os.environ["PATH"] = f"{node_bin}:{os.environ['PATH']}"
        state_track(state, "node_nvm", method="nvm", nvm_dir=str(nvm_dir), node_bin=str(node_bin))
        print(f"Node.js ready: {shutil.which('node') or node_bin / 'node'}")
    else:
        sys.exit("ERROR: Failed to install Node.js. Install manually: https://nodejs.org")


# ─── OpenClaw CLI ─────────────────────────────────────────────────────────────


def ensure_openclaw_installed(state: dict) -> None:
    """Install OpenClaw locally via npm (version-pinned, local node_modules)."""
    if OPENCLAW_BIN.exists():
        return

    pkg_json = CAPSULE_DIR / "package.json"
    if not pkg_json.exists():
        # Write package.json at runtime so ato's auto-detection doesn't add an
        # npm ci install step before node is available.
        pkg_json.write_text(json.dumps(_OPENCLAW_PACKAGE_JSON, indent=2))

    if not shutil.which("npm"):
        sys.exit(
            "ERROR: npm is required to install OpenClaw CLI.\n"
            "  Install Node.js from https://nodejs.org and re-run."
        )

    lock = CAPSULE_DIR / "package-lock.json"
    cmd = ["npm", "ci"] if lock.exists() else ["npm", "install"]
    print(f"Installing OpenClaw CLI ({' '.join(cmd)})...")
    subprocess.run(cmd, cwd=CAPSULE_DIR, check=True)
    state_track(state, "openclaw_npm", method="local_node_modules")


def _openclaw_bin() -> str:
    if OPENCLAW_BIN.exists():
        return str(OPENCLAW_BIN)
    return shutil.which("openclaw") or "openclaw"


# ─── OpenClaw Config ──────────────────────────────────────────────────────────


def write_openclaw_config(model_id: str) -> None:
    """Write ~/.openclaw/openclaw.json; always overwrite to apply active model."""
    config_dir = Path.home() / ".openclaw"
    config_path = config_dir / "openclaw.json"
    config_dir.mkdir(parents=True, exist_ok=True)
    config = {
        "gateway": {
            "mode": "local",
            "auth": {"token": GATEWAY_TOKEN},
        },
        "models": {
            "providers": {
                "ollama": {
                    "baseUrl": f"{OLLAMA_HOST}/v1",
                    "apiKey": "ollama-local",
                    "models": [
                        {
                            "id": model_id,
                            "name": model_id,
                            "reasoning": False,
                            "input": ["text"],
                            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
                            "contextWindow": 128000,
                            "maxTokens": 8192,
                        }
                    ],
                }
            }
        },
        "agents": {
            "defaults": {
                "model": {"primary": f"ollama/{model_id}"},
            }
        },
    }
    with open(config_path, "w") as f:
        json.dump(config, f, indent=2)
    print(f"Wrote OpenClaw config: {config_path}")


# ─── Model Selection ──────────────────────────────────────────────────────────


def _select_model_tui() -> str:
    """Interactive model picker using stdlib only."""
    print("\nSelect model (Enter = default: qwen3:8b):\n")
    for i, m in enumerate(MODEL_CATALOG, 1):
        tag = "  [DEFAULT]" if m.get("default") else ""
        print(f"  {i}. {m['label']}{tag}")
    print()
    while True:
        try:
            raw = input("> ").strip()
        except (EOFError, KeyboardInterrupt):
            print()
            return DEFAULT_MODEL
        if raw == "":
            return DEFAULT_MODEL
        if raw.isdigit():
            idx = int(raw) - 1
            if 0 <= idx < len(MODEL_CATALOG):
                entry = MODEL_CATALOG[idx]
                if entry["id"] == "__custom__":
                    try:
                        custom = input("Enter model id (e.g. mistral:7b): ").strip()
                    except (EOFError, KeyboardInterrupt):
                        return DEFAULT_MODEL
                    return custom if custom else DEFAULT_MODEL
                return entry["id"]
        return raw  # treat as direct model id


def resolve_model(state: dict, reset: bool = False) -> str:
    """Determine active model, running TUI if needed."""
    # 1. Env override (single model)
    env_model = os.environ.get("OPENCLAW_MODEL", "").strip()
    if env_model:
        return env_model

    # 2. Multi-model env: first entry is active
    env_models = os.environ.get("OPENCLAW_MODELS", "").strip()
    if env_models:
        return env_models.split(",")[0].strip()

    # 3. Saved choice (unless reset requested)
    if not reset and state.get("saved_model"):
        return state["saved_model"]

    # 4. Non-interactive: use default
    if (
        not sys.stdin.isatty()
        or os.environ.get("OPENCLAW_INTERACTIVE", "true").lower() == "false"
    ):
        return DEFAULT_MODEL

    # 5. Interactive TUI
    chosen = _select_model_tui()
    state["saved_model"] = chosen
    save_state(state)
    return chosen


# ─── Service Management ───────────────────────────────────────────────────────

_LAUNCHD_LABEL = f"run.{CAPSULE_ID}"
_LAUNCHD_PLIST = Path.home() / "Library" / "LaunchAgents" / f"{_LAUNCHD_LABEL}.plist"
_SYSTEMD_UNIT = (
    Path.home() / ".config" / "systemd" / "user" / f"{CAPSULE_ID}.service"
)


def stop_existing_gateway() -> None:
    """Stop any gateway already listening on GATEWAY_PORT before starting a new one."""
    import socket
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.settimeout(1)
        if s.connect_ex(("127.0.0.1", int(GATEWAY_PORT))) != 0:
            return  # port is free — nothing to stop

    print(f"Stopping existing gateway on port {GATEWAY_PORT}...")
    bin_path = _openclaw_bin()
    result = subprocess.run([bin_path, "gateway", "stop"], capture_output=True)
    if result.returncode != 0:
        # Fallback: kill by port if `gateway stop` failed
        subprocess.run(
            ["sh", "-c", f"fuser -k {GATEWAY_PORT}/tcp 2>/dev/null || true"],
            check=False,
        )
    # Wait for port to free up (up to 5s)
    for _ in range(10):
        time.sleep(0.5)
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.settimeout(0.5)
            if s.connect_ex(("127.0.0.1", int(GATEWAY_PORT))) != 0:
                return
    print("⚠  Port still in use after stop attempt; proceeding anyway.")


def _gateway_argv() -> list[str]:
    bin_path = _openclaw_bin()
    return [
        bin_path,
        "gateway",
        "--port", GATEWAY_PORT,
        "--allow-unconfigured",
        "--token", GATEWAY_TOKEN,
    ]


def _install_launchd(state: dict) -> None:
    _LAUNCHD_PLIST.parent.mkdir(parents=True, exist_ok=True)
    log_dir = REAL_HOME / ".ato" / CAPSULE_ID
    log_dir.mkdir(parents=True, exist_ok=True)
    args_xml = "".join(f"<string>{c}</string>" for c in _gateway_argv())
    plist = (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"'
        ' "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n'
        '<plist version="1.0"><dict>\n'
        f'  <key>Label</key><string>{_LAUNCHD_LABEL}</string>\n'
        f'  <key>ProgramArguments</key><array>{args_xml}</array>\n'
        '  <key>RunAtLoad</key><true/>\n'
        '  <key>KeepAlive</key><true/>\n'
        f'  <key>StandardOutPath</key><string>{log_dir}/gateway.log</string>\n'
        f'  <key>StandardErrorPath</key><string>{log_dir}/gateway.err</string>\n'
        '</dict></plist>\n'
    )
    _LAUNCHD_PLIST.write_text(plist)
    subprocess.run(["launchctl", "load", str(_LAUNCHD_PLIST)], check=False)
    state_track(state, "service", platform="macos", type="launchd", plist=str(_LAUNCHD_PLIST))
    print(f"✓ launchd service installed: {_LAUNCHD_PLIST}")


def _install_systemd_user(state: dict) -> None:
    _SYSTEMD_UNIT.parent.mkdir(parents=True, exist_ok=True)
    cmd_str = " ".join(_gateway_argv())
    unit = (
        "[Unit]\n"
        f"Description=OpenClaw Gateway ({CAPSULE_ID})\n"
        "After=network.target\n\n"
        "[Service]\n"
        f"ExecStart={cmd_str}\n"
        "Restart=on-failure\n"
        f"Environment=OLLAMA_HOST={OLLAMA_HOST}\n"
        "Environment=OLLAMA_API_KEY=ollama-local\n"
        f"Environment=OPENCLAW_GATEWAY_TOKEN={GATEWAY_TOKEN}\n\n"
        "[Install]\n"
        "WantedBy=default.target\n"
    )
    _SYSTEMD_UNIT.write_text(unit)
    subprocess.run(["systemctl", "--user", "daemon-reload"], check=False)
    subprocess.run(["systemctl", "--user", "enable", f"{CAPSULE_ID}.service"], check=False)
    subprocess.run(["systemctl", "--user", "start", f"{CAPSULE_ID}.service"], check=False)
    state_track(state, "service", platform="linux", type="systemd_user", unit=str(_SYSTEMD_UNIT))
    print(f"✓ systemd user service installed: {_SYSTEMD_UNIT}")


def install_service(service_mode: str, plat: str, state: dict) -> None:
    if service_mode not in ("service", "autostart"):
        return
    print(f"Installing as {service_mode} (platform: {plat})...")
    if plat == "macos":
        _install_launchd(state)
    elif plat in ("ubuntu", "linux", "wsl2"):
        if wsl_has_systemd():
            _install_systemd_user(state)
        else:
            print("⚠  systemd not available. Re-run `ato run` to start manually.")
    else:
        print(f"⚠  Service install not supported on platform: {plat}")


def _uninstall_node(state: dict) -> None:
    node_info = state["installed"].get("node_nvm", {})
    if not node_info.get("installed_by_us"):
        return
    nvm_dir = Path(node_info.get("nvm_dir", str(REAL_HOME / ".nvm")))
    if nvm_dir.exists():
        shutil.rmtree(nvm_dir, ignore_errors=True)
        print(f"✓ Removed nvm/Node.js: {nvm_dir}")


def _uninstall_service(state: dict) -> None:
    svc = state["installed"].get("service", {})
    if not svc.get("installed_by_us"):
        return
    svc_type = svc.get("type")
    if svc_type == "launchd":
        plist = Path(svc.get("plist", str(_LAUNCHD_PLIST)))
        subprocess.run(["launchctl", "unload", str(plist)], check=False)
        plist.unlink(missing_ok=True)
        print(f"✓ Removed launchd service: {plist}")
    elif svc_type == "systemd_user":
        unit = Path(svc.get("unit", str(_SYSTEMD_UNIT)))
        subprocess.run(["systemctl", "--user", "stop", f"{CAPSULE_ID}.service"], check=False)
        subprocess.run(["systemctl", "--user", "disable", f"{CAPSULE_ID}.service"], check=False)
        unit.unlink(missing_ok=True)
        subprocess.run(["systemctl", "--user", "daemon-reload"], check=False)
        print(f"✓ Removed systemd user service: {unit}")


def _other_ollama_users() -> list[str]:
    """Return names of other capsule state files that also track ollama_bin."""
    users = []
    state_dir = REAL_HOME / ".openclaw"
    if not state_dir.exists():
        return users
    for path in state_dir.glob(".*.state.json"):
        if path == STATE_FILE:
            continue
        try:
            s = json.loads(path.read_text())
            if s.get("installed", {}).get("ollama_bin", {}).get("installed_by_us"):
                users.append(path.name)
        except Exception:
            pass
    return users


# ─── Commands ─────────────────────────────────────────────────────────────────


def cmd_uninstall(state: dict) -> None:
    print(f"Uninstalling {CAPSULE_ID}...")
    _uninstall_service(state)

    # Remove local openclaw node_modules and generated package files
    if state_has(state, "openclaw_npm"):
        nm = CAPSULE_DIR / "node_modules"
        if nm.exists():
            shutil.rmtree(nm)
            print(f"✓ Removed node_modules: {nm}")
        for fname in ("package-lock.json", "package.json"):
            p = CAPSULE_DIR / fname
            if p.exists():
                p.unlink()

    # Remove Node.js (nvm) if we installed it
    _uninstall_node(state)

    # Remove ollama models we pulled
    models_to_remove = [
        k.split(":", 1)[1]
        for k, v in state["installed"].items()
        if k.startswith("ollama_model:") and v.get("installed_by_us")
    ]
    if models_to_remove and ollama_ready():
        for m in models_to_remove:
            print(f"Removing model {m}...")
            subprocess.run(["ollama", "rm", m], check=False)
            print(f"✓ Removed model: {m}")

    # Remove ollama binary only if no other capsule needs it
    if state_has(state, "ollama_bin"):
        other = _other_ollama_users()
        if other:
            print(f"⚠  Ollama kept (also used by: {', '.join(other)})")
        else:
            info = state["installed"]["ollama_bin"]
            method, plat = info.get("method", ""), info.get("platform", "")
            if method == "brew" and shutil.which("brew"):
                subprocess.run(["brew", "uninstall", "ollama"], check=False)
                print("✓ Removed Ollama (brew)")
            elif plat in ("ubuntu", "linux", "wsl2"):
                ollama_path = shutil.which("ollama")
                if ollama_path:
                    try:
                        os.remove(ollama_path)
                    except PermissionError:
                        subprocess.run(["sudo", "rm", ollama_path], check=False)
                    print(f"✓ Removed Ollama binary: {ollama_path}")

    STATE_FILE.unlink(missing_ok=True)
    print("Uninstall complete.")


def cmd_add_model(model_id: str, state: dict) -> None:
    """Pull an extra model and track it; does not start the gateway."""
    plat = detect_platform()
    ensure_ollama_installed(plat, state)
    start_ollama(state)
    pull_model(model_id, state)
    print(f"\nModel '{model_id}' downloaded.")
    print(f"Run without --add-model to start the gateway.")
    print(f"Set OPENCLAW_MODEL={model_id} to use it as the active model.")


def cmd_list_models(state: dict) -> None:
    tracked = {
        k.split(":", 1)[1]: v
        for k, v in state["installed"].items()
        if k.startswith("ollama_model:")
    }
    local = list_local_ollama_models()
    saved = state.get("saved_model", "")

    print("Tracked models (installed by this capsule):")
    for model_id, _info in tracked.items():
        active = "  ← active" if model_id == saved else ""
        print(f"  ● {model_id}{active}")
    if not tracked:
        print("  (none)")

    untracked = [m for m in local if m not in tracked]
    if untracked:
        print("\nOther local Ollama models:")
        for m in untracked:
            print(f"  ○ {m}")


# ─── Main ─────────────────────────────────────────────────────────────────────


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description="OpenClaw + Ollama bootstrap",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Env vars:\n"
            "  OPENCLAW_MODEL         Override active model (single)\n"
            "  OPENCLAW_MODELS        Comma-separated list; first is active\n"
            "  OPENCLAW_INTERACTIVE   Set 'false' to skip model TUI\n"
            "  OPENCLAW_SERVICE_MODE  oneshot | service | autostart\n"
            "  OPENCLAW_RESET_MODEL   Set '1' to force model re-selection\n"
        ),
    )
    p.add_argument("--add-model", metavar="MODEL", help="Pull a model and exit")
    p.add_argument("--list-models", action="store_true", help="List tracked and local models")
    p.add_argument("--uninstall", action="store_true", help="Remove everything installed by this capsule")
    p.add_argument("--reset-model", action="store_true", help="Force interactive model re-selection")
    return p.parse_args()


def main() -> None:
    args = parse_args()
    state = load_state()
    plat = detect_platform()

    if args.uninstall:
        cmd_uninstall(state)
        return

    if args.list_models:
        cmd_list_models(state)
        return

    ensure_ollama_installed(plat, state)
    start_ollama(state)

    if args.add_model:
        cmd_add_model(args.add_model, state)
        return

    reset = args.reset_model or os.environ.get("OPENCLAW_RESET_MODEL", "") == "1"
    model_id = resolve_model(state, reset=reset)

    # Pull active model + any extras declared via OPENCLAW_MODELS
    env_models = os.environ.get("OPENCLAW_MODELS", "").strip()
    if env_models:
        for em in (m.strip() for m in env_models.split(",") if m.strip()):
            pull_model(em, state)
    else:
        pull_model(model_id, state)

    ensure_node_installed(state)
    ensure_openclaw_installed(state)
    write_openclaw_config(model_id)

    service_mode = os.environ.get("OPENCLAW_SERVICE_MODE", "oneshot")
    install_service(service_mode, plat, state)

    os.environ["OLLAMA_API_KEY"] = "ollama-local"
    os.environ["OLLAMA_HOST"] = OLLAMA_HOST
    os.environ["OPENCLAW_GATEWAY_TOKEN"] = GATEWAY_TOKEN

    bin_path = _openclaw_bin()
    print(f"Starting OpenClaw gateway (model: ollama/{model_id})")
    print(f"Dashboard: http://127.0.0.1:{GATEWAY_PORT}/#token={GATEWAY_TOKEN}")
    stop_existing_gateway()
    os.execvp(bin_path, [
        bin_path, "gateway",
        "--port", GATEWAY_PORT,
        "--allow-unconfigured",
        "--token", GATEWAY_TOKEN,
    ])


if __name__ == "__main__":
    main()
