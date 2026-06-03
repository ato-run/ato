"""Generate the #369 matrix CSV + per-app receipt.md from result.json + the
operator's (inspected-screenshot) classifications. Run after run_matrix.py."""
import csv
import json
from pathlib import Path

DATE = "20260604"
ROOT = Path(r"C:\Users\koh\ato")
RDIR = ROOT / "docs" / "aodd" / "receipts" / f"desktop-recipe-launch-{DATE}"
MATRIX = ROOT / "docs" / "aodd" / "desktop_recipe_launch_matrix.csv"

PLATFORM = "windows-x86_64"
DESKTOP_BUILD = "ato-desktop debug v0.5.5 (dev 0b3f9826, post-#441)"
ATO_CLI_BUILD = "target/debug/ato.exe v0.5.5 (post-#441, source_build_shell_unavailable marker present)"
ATO_HOME = r"C:\Users\koh\AppData\Local\Temp\aodd-369-home (clean temp)"
PROVIDER = "Podman 5.8.2 (podman-machine-default, WSL, rootless)"

# Operator classifications, keyed by slug. Built by inspecting each captured
# screenshot + result.json + desktop.log (honest AODD judgment, not automated).
C = {
    "memos": dict(tier="A", port=5230, runtime="OCI single container (Go+SQLite)",
        result="PASS", consent="auto (no prompt)", provider="podman ready",
        webview="app UI (Memos signup form)", blocker="", follow="",
        notes="GitHub URL resolved to catalog OCI recipe (no /bin/sh). Memos 'create account' screen rendered. Container clean after stop."),
    "uptime-kuma": dict(tier="A", port=3001, runtime="OCI single container (Node+SQLite)",
        result="PASS", consent="auto", provider="podman ready",
        webview="app UI (Admin account setup)", blocker="", follow="",
        notes="Setup wizard rendered. Container clean after stop."),
    "pocketbase": dict(tier="A", port=8090, runtime="OCI single container (Go)",
        result="DEGRADED", consent="auto", provider="podman ready",
        webview="404 JSON at / (admin UI is at /_/)", blocker="WebView entry path is '/' which PocketBase answers with {\"message\":\"File not found\",\"status\":404}; the dashboard lives at /_/",
        follow="#447 (pocketbase entry path /_/)",
        notes="Container up + WebView renders, but lands on a 404 instead of the admin UI."),
    "homepage": dict(tier="A", port=3000, runtime="OCI single container (Node)",
        result="DEGRADED", consent="auto", provider="podman ready",
        webview="app error page: 'Host validation failed. See logs for more details.'",
        blocker="homepage rejects the 127.0.0.1:<port> Host header; recipe does not set HOMEPAGE_ALLOWED_HOSTS",
        follow="#446 (HOMEPAGE_ALLOWED_HOSTS)",
        notes="WebView renders homepage's own error screen rather than the dashboard."),
    "node-red": dict(tier="A", port=1880, runtime="OCI single container (Node)",
        result="FAIL", consent="auto", provider="podman ready",
        webview="none (no guest-capsule pane; container exited before ready)",
        blocker="container exits at startup: EPERM: operation not permitted, copyfile '/usr/src/node-red/node_modules/node-red/settings.js' -> '/data/settings.js' (state bind-mount not writable by container user on rootless Podman/WSL)",
        follow="#444 (Windows/Podman state bind-mount ownership)",
        notes="No container at ready or after stop (clean). Same mount-permission class as blinko."),
    "fresh-rss": dict(tier="A", port=80, runtime="OCI single container (PHP)",
        result="PASS", consent="auto", provider="podman ready",
        webview="app UI (FreshRSS install wizard step 1, v1.29.1)", blocker="", follow="",
        notes="Install wizard rendered. Container clean after stop."),
    "linkwarden": dict(tier="A", port=3000, runtime="app+postgres (intended)",
        result="SKIPPED_MISSING_RECIPE", consent="n/a", provider="n/a",
        webview="none",
        blocker="not registered in the bundled sample-recipe catalog (SAMPLE_RECIPE_CATALOG); GitHub handle resolved to raw source-build, which has no capsule.toml -> preflight failed",
        follow="#449 (register linkwarden/langflow in catalog)",
        notes="Confirms the #377 split from the other side: unregistered handles take the raw GitHub source-build path."),
    "blinko": dict(tier="A", port=1111, runtime="OCI 2-service (app + postgres:14)",
        result="FAIL", consent="auto", provider="podman ready",
        webview="none (db service exited before ready)",
        blocker="postgres 'db' service exits: chmod: changing permissions of '/var/lib/postgresql/data': Operation not permitted; initdb cannot fix permissions on the bind-mounted data dir (rootless Podman/WSL). Surfaced as E999 'orchestration services failed to start in-process' (cause: service 'db' exited before readiness check passed).",
        follow="#444 (state bind-mount ownership) + #445 (E999 vs typed exited-before-ready)",
        notes="Same mount-permission root cause as node-red. No containers after stop (clean)."),
    "langflow": dict(tier="A", port=7860, runtime="Python/container (intended)",
        result="SKIPPED_MISSING_RECIPE", consent="n/a", provider="n/a",
        webview="none",
        blocker="not registered in the bundled sample-recipe catalog; GitHub handle resolved to raw source-build, capsule.toml absent -> preflight failed (consent wizard shows error state)",
        follow="#449 (register linkwarden/langflow in catalog)",
        notes="Same class as linkwarden."),
    # Tier B
    "excalidraw": dict(tier="B", port=8080, runtime="OCI single container (nginx static)",
        result="PASS", consent="auto", provider="podman ready",
        webview="app UI (Excalidraw canvas + toolbar)", blocker="", follow="",
        notes="#377 regression anchor: GitHub URL -> catalog OCI recipe, NO /bin/sh source-build. Full Excalidraw UI rendered. Clean after stop."),
    "n8n": dict(tier="B", port=5678, runtime="OCI single container (Node+SQLite)",
        result="DEGRADED", consent="auto", provider="podman ready",
        webview="app splash 'n8n is starting up. Please wait' (editor not yet loaded)",
        blocker="readiness probe passes on n8n's HTTP 'starting up' splash before the editor/setup UI is ready; screenshot captured the splash, not demo-ready UI",
        follow="#448 (n8n readiness vs startup splash)",
        notes="Container up, WebView serves n8n's own content (not blank/error). Slow first boot (~145s)."),
    "pgweb": dict(tier="B", port=8081, runtime="OCI single container (Go)",
        result="PASS", consent="auto", provider="podman ready",
        webview="app UI (pgweb v0.17.0 connection form)", blocker="", follow="",
        notes="#377 regression anchor. Connection form rendered. Clean after stop."),
    "adminer": dict(tier="B", port=8080, runtime="OCI single container (PHP)",
        result="PASS", consent="auto", provider="podman ready",
        webview="app UI (Adminer 4.8.1 login form)", blocker="", follow="",
        notes="Login form rendered. Clean after stop."),
    "openlist": dict(tier="B", port=5244, runtime="OCI (openlist-google-drive-crypt)",
        result="SKIPPED_UNSUITABLE", consent="secret required", provider="podman ready",
        webview="none",
        blocker="recipe requires 1 secret (Google Drive crypt config); automated run did not provide it ('1 required secret(s) - run: ato app config set github.com/openlistteam/openlist')",
        follow="",
        notes="Resolves to the openlist-google-drive-crypt catalog recipe; needs external Google Drive credentials, out of scope for an unattended AODD run."),
    "open-webui": dict(tier="B", port=8080, runtime="OCI single container (heavy, ML)",
        result="SKIPPED_PLATFORM_BLOCKED", consent="auto", provider="podman ready",
        webview="none (did not reach Ready within 200s)",
        blocker="4.82GB image present, boot wizard opened, but no container reached Ready / no WebView pane within 200s on the memory-constrained 2GB Podman WSL machine",
        follow="",
        notes="Not a recipe/launch-path defect; resource-constrained host. Re-test on a machine with more RAM allocated to the Podman VM."),
}


def webview_url(slug):
    f = RDIR / slug / "result.json"
    if f.exists():
        r = json.loads(f.read_text(encoding="utf-8"))
        return r.get("webview_url") or "", r.get("ready_detected_after_s")
    return "", None


def orphan_for(slug):
    f = RDIR / slug / "result.json"
    if not f.exists():
        return "n/a"
    r = json.loads(f.read_text(encoding="utf-8"))
    after = (r.get("podman_after_stop") or {}).get("containers", "")
    own = [ln for ln in after.splitlines() if slug in ln]
    return "containers clean" if not own else f"LEFTOVER: {own}"


COLS = ["slug", "repo", "tier", "platform", "desktop_build", "ato_cli_build",
        "ato_home", "recipe_source", "expected_runtime_shape", "expected_port",
        "provider", "result", "desktop_launch", "consent_flow", "provider_flow",
        "session_ready", "webview_rendered", "stop_from_desktop", "orphan_check",
        "screenshot_path", "logs_path", "receipt_path", "first_blocker",
        "follow_up_issue", "notes"]

REPOS = {
    "memos": "usememos/memos", "uptime-kuma": "louislam/uptime-kuma",
    "pocketbase": "pocketbase/pocketbase", "homepage": "gethomepage/homepage",
    "node-red": "node-red/node-red", "fresh-rss": "FreshRSS/FreshRSS",
    "linkwarden": "linkwarden/linkwarden", "blinko": "blinkospace/blinko",
    "langflow": "langflow-ai/langflow", "excalidraw": "excalidraw/excalidraw",
    "n8n": "n8n-io/n8n", "pgweb": "sosedoff/pgweb", "adminer": "vrana/adminer",
    "openlist": "openlistteam/openlist", "open-webui": "open-webui/open-webui",
}

ORDER = ["memos", "uptime-kuma", "pocketbase", "homepage", "node-red", "fresh-rss",
         "linkwarden", "blinko", "langflow", "excalidraw", "n8n", "pgweb",
         "adminer", "openlist", "open-webui"]

rows = []
for slug in ORDER:
    c = C[slug]
    url, ready = webview_url(slug)
    has_shot = (RDIR / slug / "screenshot.png").exists()
    pass_like = c["result"] in ("PASS", "DEGRADED")
    rel = f"docs/aodd/receipts/desktop-recipe-launch-{DATE}/{slug}"
    row = {
        "slug": slug, "repo": REPOS[slug], "tier": c["tier"], "platform": PLATFORM,
        "desktop_build": DESKTOP_BUILD, "ato_cli_build": ATO_CLI_BUILD,
        "ato_home": ATO_HOME, "recipe_source": f"capsule://github.com/{REPOS[slug]}",
        "expected_runtime_shape": c["runtime"], "expected_port": c["port"],
        "provider": PROVIDER, "result": c["result"],
        "desktop_launch": "YES (omnibar NavigateToUrl)",
        "consent_flow": c["consent"], "provider_flow": c["provider"],
        "session_ready": (f"YES ({ready}s)" if ready else "NO"),
        "webview_rendered": c["webview"],
        "stop_from_desktop": "YES" if pass_like else ("YES (no active session)" if c["result"].startswith("SKIPPED") or c["result"] == "FAIL" else "NO"),
        "orphan_check": orphan_for(slug) + "; NOTE: per-session podman network left (cross-cutting, #450)",
        "screenshot_path": f"{rel}/screenshot.png" if has_shot else "",
        "logs_path": f"{rel}/",
        "receipt_path": f"{rel}/receipt.md",
        "first_blocker": c["blocker"], "follow_up_issue": c["follow"],
        "notes": c["notes"],
    }
    rows.append(row)

with open(MATRIX, "w", newline="", encoding="utf-8") as f:
    w = csv.DictWriter(f, fieldnames=COLS)
    w.writeheader()
    w.writerows(rows)
print(f"wrote {MATRIX} ({len(rows)} rows)")

# Per-app receipt.md
TPL = """App: {slug}
Repo: {repo}
Platform: {platform}
Desktop build: {desktop_build}
Ato CLI build: {ato_cli_build}
Git SHA: 0b3f9826c7852f6c46731c0c6b537395062ed0f6 (dev, post-#441)
ATO_HOME: {ato_home}
Provider: {provider}
Launch path used: Desktop omnibar -> NavigateToUrl {recipe_source}
Recipe source: {recipe_source} ({runtime})
Expected runtime shape: {runtime}
Prompts observed: {consent}
Provider flow: {provider_flow}
Consent flow: {consent}
Secret flow: {secret}
Ready signal: {ready}
WebView URL: {url}
Screenshot: {shot}
Stop result: {stop}
Orphan check: {orphan}
Result: {result}
First blocker: {blocker}
Follow-up issue: {follow}
Notes: {notes}
"""
for slug in ORDER:
    c = C[slug]
    url, ready = webview_url(slug)
    d = RDIR / slug
    d.mkdir(parents=True, exist_ok=True)
    (d / "receipt.md").write_text(TPL.format(
        slug=slug, repo=REPOS[slug], platform=PLATFORM, desktop_build=DESKTOP_BUILD,
        ato_cli_build=ATO_CLI_BUILD, ato_home=ATO_HOME, provider=PROVIDER,
        recipe_source=f"capsule://github.com/{REPOS[slug]}", runtime=c["runtime"],
        consent=c["consent"], provider_flow=c["provider"],
        secret=("required (Google Drive crypt) - not provided" if slug == "openlist" else "none / not reached"),
        ready=(f"guest-capsule pane bound to {url} after {ready}s" if ready else "NOT reached (no guest-capsule pane)"),
        url=url or "n/a", shot=("screenshot.png" if (d / 'screenshot.png').exists() else "n/a"),
        stop=("stopped=true" if c["result"] in ("PASS", "DEGRADED") else "no active session to stop"),
        orphan=orphan_for(slug) + "; per-session podman network left behind (cross-cutting cleanup gap, #450)",
        result=c["result"], blocker=c["blocker"] or "none", follow=c["follow"] or "none",
        notes=c["notes"]), encoding="utf-8")
print("wrote per-app receipts")
