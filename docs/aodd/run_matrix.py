"""
AODD Desktop Recipe Launch driver (issue #369 rerun, post-#441).

Drives capsule launches through the Ato Desktop automation surface (the same
omnibar `NavigateToUrl` a user types), captures a WebView screenshot, records
Podman container/process state before and after stop, and writes per-app
artifacts under a dated receipts directory.

It does NOT decide PASS/FAIL — the operator (human/agent) inspects the captured
screenshots and logs and classifies each row. This keeps the judgment honest:
a screenshot being *captured* is not proof the WebView *rendered* app UI.

Usage:
  python run_matrix.py --socket <pipe> --out <dir> --apps memos,excalidraw,pgweb
"""
import argparse
import base64
import datetime
import json
import subprocess
import time
from pathlib import Path

ATO_ROOT = Path(r"C:\Users\koh\ato")
MCP_BIN = ATO_ROOT / "crates" / "ato-desktop" / "target" / "debug" / "ato-desktop-mcp.exe"

# slug -> (repo, capsule url). URL is the GitHub form so we also exercise the
# #377 catalog-vs-source-build resolution path.
APPS = {
    # Tier A
    "memos": ("usememos/memos", "capsule://github.com/usememos/memos"),
    "uptime-kuma": ("louislam/uptime-kuma", "capsule://github.com/louislam/uptime-kuma"),
    "pocketbase": ("pocketbase/pocketbase", "capsule://github.com/pocketbase/pocketbase"),
    "homepage": ("gethomepage/homepage", "capsule://github.com/gethomepage/homepage"),
    "node-red": ("node-red/node-red", "capsule://github.com/node-red/node-red"),
    "fresh-rss": ("FreshRSS/FreshRSS", "capsule://github.com/FreshRSS/FreshRSS"),
    "blinko": ("blinkospace/blinko", "capsule://github.com/blinkospace/blinko"),
    "linkwarden": ("linkwarden/linkwarden", "capsule://github.com/linkwarden/linkwarden"),
    "langflow": ("langflow-ai/langflow", "capsule://github.com/langflow-ai/langflow"),
    # Tier B
    "excalidraw": ("excalidraw/excalidraw", "capsule://github.com/excalidraw/excalidraw"),
    "n8n": ("n8n-io/n8n", "capsule://github.com/n8n-io/n8n"),
    "pgweb": ("sosedoff/pgweb", "capsule://github.com/sosedoff/pgweb"),
    "adminer": ("vrana/adminer", "capsule://github.com/vrana/adminer"),
    "openlist": ("openlistteam/openlist", "capsule://github.com/openlistteam/openlist"),
    "open-webui": ("open-webui/open-webui", "capsule://github.com/open-webui/open-webui"),
}


class MCP:
    def __init__(self, socket):
        cmd = [str(MCP_BIN)]
        if socket:
            cmd += ["--socket", socket]
        self.p = subprocess.Popen(
            cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, text=True, bufsize=1,
            encoding="utf-8", errors="replace",
        )
        self.id = 0
        self.call("initialize", {
            "protocolVersion": "2024-11-05", "capabilities": {},
            "clientInfo": {"name": "aodd-369", "version": "2.0"},
        }, raw=True)

    def call(self, method, params=None, raw=False):
        self.id += 1
        req = {"jsonrpc": "2.0", "id": self.id, "method": method}
        if params is not None:
            req["params"] = params
        self.p.stdin.write(json.dumps(req) + "\n")
        self.p.stdin.flush()
        line = self.p.stdout.readline()
        if not line:
            return None
        try:
            return json.loads(line.strip())
        except json.JSONDecodeError:
            return {"raw": line.strip()}

    def tool(self, name, args=None):
        params = {"name": name}
        if args:
            params["arguments"] = args
        return self.call("tools/call", params)

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=5)
        except Exception:
            self.p.kill()


def podman_state():
    """Snapshot podman containers + ato networks for orphan checks."""
    out = {}
    for key, cmd in {
        "containers": ["podman", "ps", "-a", "--format", "{{.Names}}\t{{.Status}}\t{{.Image}}"],
        "networks": ["podman", "network", "ls", "--format", "{{.Name}}"],
    }.items():
        try:
            r = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
            out[key] = r.stdout.strip()
        except Exception as e:
            out[key] = f"ERR: {e}"
    return out


def _b64(s):
    try:
        return base64.b64decode(s)
    except Exception:
        return None


def extract_screenshot_bytes(resp):
    """MCP screenshot tool returns a text content block whose text is JSON
    `{"data": "<base64 png>"}`. Also handle a direct image block."""
    if not resp or "result" not in resp:
        return None
    result = resp["result"]
    for block in (result.get("content") or []) if isinstance(result, dict) else []:
        if not isinstance(block, dict):
            continue
        if block.get("type") == "image" and block.get("data"):
            return _b64(block["data"])
        if block.get("type") == "text" and block.get("text"):
            try:
                inner = json.loads(block["text"])
                if isinstance(inner, dict) and inner.get("data"):
                    return _b64(inner["data"])
            except json.JSONDecodeError:
                pass
    return None


def tabs_payload(resp):
    """Parse the browser_tabs text block into the panes dict."""
    if not resp or "result" not in resp:
        return None
    for block in (resp["result"].get("content") or []):
        if isinstance(block, dict) and block.get("type") == "text":
            try:
                return json.loads(block["text"])
            except json.JSONDecodeError:
                return None
    return None


def drive(mcp, slug, repo, url, out_dir, ready_wait):
    app_dir = out_dir / slug
    app_dir.mkdir(parents=True, exist_ok=True)
    rec = {"slug": slug, "repo": repo, "url": url, "steps": []}

    def step(name, resp):
        snippet = json.dumps(resp, ensure_ascii=False)[:600] if resp else "None"
        rec["steps"].append({"step": name, "resp": snippet})
        print(f"  [{slug}] {name}: {snippet[:200]}")

    rec["podman_before"] = podman_state()

    # 1. NavigateToUrl — the omnibar user action.
    step("NavigateToUrl", mcp.tool("host_dispatch_action", {"action": "NavigateToUrl", "url": url}))
    time.sleep(4)

    # 2. Observe + try to clear any consent prompt the same way a user would.
    step("snapshot_after_nav", mcp.tool("browser_snapshot"))
    step("ForceApprovePending", mcp.tool("host_dispatch_action", {"action": "ForceApprovePending"}))

    # 3. Wait for readiness: a guest-capsule pane bound to a localhost URL is
    #    the real Desktop "session ready + WebView created" signal.
    waited = 0
    interval = 5
    while waited < ready_wait:
        time.sleep(interval)
        waited += interval
        tp = tabs_payload(mcp.tool("browser_tabs"))
        panes = (tp or {}).get("panes", []) if isinstance(tp, dict) else []
        guest = next((p for p in panes if p.get("kind") == "guest-capsule"
                      and str(p.get("url", "")).startswith("http")), None)
        rec.setdefault("tab_polls", []).append(json.dumps(tp, ensure_ascii=False)[:300])
        if guest:
            rec["ready_detected_after_s"] = waited
            rec["webview_url"] = guest.get("url")
            break

    # 4. Screenshot the WebView (give the page a moment to paint).
    time.sleep(3)
    shot = mcp.tool("browser_take_screenshot")
    png = extract_screenshot_bytes(shot)
    if png:
        (app_dir / "screenshot.png").write_bytes(png)
        rec["screenshot"] = "screenshot.png"
        rec["screenshot_bytes"] = len(png)
    else:
        rec["screenshot"] = None
        step("screenshot_raw", shot)

    # Console messages help explain blank/error WebViews.
    cons = mcp.tool("browser_console_messages")
    rec["console"] = json.dumps(cons, ensure_ascii=False)[:1500] if cons else None
    step("browser_tabs_final", mcp.tool("browser_tabs"))

    rec["podman_running_at_ready"] = podman_state()

    # 5. Stop from Desktop.
    step("stop_active_session", mcp.tool("stop_active_session"))
    time.sleep(5)
    rec["podman_after_stop"] = podman_state()

    (app_dir / "result.json").write_text(json.dumps(rec, ensure_ascii=False, indent=2), encoding="utf-8")
    return rec


def discover_socket(home):
    """Read the live automation socket from <home>/run/ato-desktop-current.json.

    Passing the `\\.\pipe\...` string through bash + argparse mangles
    backslashes, so we read it from the file the Desktop wrote instead.
    """
    cur = Path(home) / "run" / "ato-desktop-current.json"
    if cur.exists():
        return json.loads(cur.read_text(encoding="utf-8")).get("socket", "")
    return ""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--socket", default=None)
    ap.add_argument("--home", default=None, help="ATO_HOME dir to discover socket from")
    ap.add_argument("--out", required=True)
    ap.add_argument("--apps", required=True, help="comma-separated slugs")
    ap.add_argument("--ready-wait", type=int, default=60)
    args = ap.parse_args()
    if not args.socket and args.home:
        args.socket = discover_socket(args.home)
    print(f"Using socket: {args.socket!r}")
    if not args.socket:
        raise SystemExit("no socket (desktop not running?)")

    out_dir = Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)
    mcp = MCP(args.socket)
    try:
        tools = mcp.call("tools/list")
        (out_dir / "_tools.json").write_text(json.dumps(tools, ensure_ascii=False, indent=2), encoding="utf-8")
        names = [t["name"] for t in tools.get("result", {}).get("tools", [])] if tools else []
        print(f"MCP tools: {names}")

        summary = []
        for slug in [s.strip() for s in args.apps.split(",") if s.strip()]:
            if slug not in APPS:
                print(f"  unknown slug {slug}, skipping")
                continue
            repo, url = APPS[slug]
            rec = drive(mcp, slug, repo, url, out_dir, args.ready_wait)
            summary.append({
                "slug": slug,
                "screenshot": rec.get("screenshot"),
                "screenshot_bytes": rec.get("screenshot_bytes"),
                "ready_after_s": rec.get("ready_detected_after_s"),
            })
            time.sleep(3)
        (out_dir / "_summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8")
        print(f"\nSUMMARY: {json.dumps(summary, ensure_ascii=False)}")
    finally:
        mcp.close()


if __name__ == "__main__":
    main()
