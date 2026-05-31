#!/usr/bin/env python3
"""
Publish AODD-success sample capsule.toml entries to the community registry.

Usage:
    python3 scripts/community_publish_successful_samples.py --dry-run
    python3 scripts/community_publish_successful_samples.py --yes
    python3 scripts/community_publish_successful_samples.py --yes --force   # re-publish even if already indexed

Reads docs/aodd/community-publish/successful-samples.toml for the manifest.
Writes docs/aodd/community-publish/published-samples-YYYYMMDD.json on success.

The script shells out to the ato CLI rather than duplicating HTTP logic.
"""
import argparse
import hashlib
import json
import os
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request
from datetime import date
from pathlib import Path

try:
    import tomllib  # Python 3.11+
except ModuleNotFoundError:
    try:
        import tomli as tomllib  # pip install tomli
    except ModuleNotFoundError:
        print(
            "error: Python 3.11+ or `pip install tomli` required for TOML parsing",
            file=sys.stderr,
        )
        sys.exit(1)

REPO_ROOT = Path(__file__).parent.parent
MANIFEST_PATH = REPO_ROOT / "docs/aodd/community-publish/successful-samples.toml"
API_BASE = os.environ.get("ATO_COMMUNITY_API_URL", "https://api.ato.run")
ATO_BIN = os.environ.get("ATO_BIN", "ato")


def load_manifest() -> list[dict]:
    with open(MANIFEST_PATH, "rb") as f:
        data = tomllib.load(f)
    return data.get("samples", [])


def toml_digest(toml_path: Path) -> str:
    return hashlib.sha256(toml_path.read_bytes()).hexdigest()[:16]


def fetch_existing_candidates(normalized_source: str) -> list[dict]:
    encoded = urllib.parse.quote(normalized_source, safe="")
    url = f"{API_BASE}/v1/capsule-tomls?source={encoded}"
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "ato-publish-script"})
        with urllib.request.urlopen(req, timeout=10) as resp:
            body = json.loads(resp.read())
            return body.get("candidates", [])
    except Exception as e:
        print(f"  warning: could not check existing candidates: {e}", file=sys.stderr)
        return []


def already_published(source: str, toml_path: Path) -> str | None:
    """Return ctoml_id if an equivalent candidate already exists, else None."""
    candidates = fetch_existing_candidates(source)
    digest = toml_digest(toml_path)
    for c in candidates:
        url = c.get("capsuleTomlUrl", "")
        if not url:
            continue
        # Fetch raw TOML and compare digest
        try:
            req = urllib.request.Request(url, headers={"User-Agent": "ato-publish-script"})
            with urllib.request.urlopen(req, timeout=10) as resp:
                remote_digest = hashlib.sha256(resp.read()).hexdigest()[:16]
            if remote_digest == digest:
                return c["id"]
        except Exception:
            pass
    return None


def publish_sample(sample: dict, dry_run: bool, force: bool) -> dict | None:
    slug = sample["slug"]
    source = sample["source"]
    toml_rel = sample["toml_path"]
    toml_path = REPO_ROOT / toml_rel

    print(f"\n── {slug} ({source})")

    if not toml_path.exists():
        print(f"  error: toml_path not found: {toml_path}", file=sys.stderr)
        return {"slug": slug, "status": "error", "reason": "toml_not_found"}

    if not force:
        existing_id = already_published(source, toml_path)
        if existing_id:
            print(f"  skipped: already published as {existing_id}")
            return {
                "slug": slug,
                "source": source,
                "status": "already_published",
                "ctoml_id": existing_id,
            }

    if dry_run:
        print(f"  [dry-run] would run: {ATO_BIN} community submit github.com/{source} -T {toml_rel} -y")
        print(f"  [dry-run] toml digest: {toml_digest(toml_path)}")
        return None

    # github.com/<source> is the input format the CLI expects
    cmd = [
        ATO_BIN,
        "community",
        "submit",
        f"github.com/{source}",
        "-T",
        str(toml_path),
        "-y",
    ]
    env = {**os.environ, "ATO_COMMUNITY_API_URL": API_BASE}
    result = subprocess.run(cmd, capture_output=True, text=True, env=env)

    if result.returncode != 0:
        print(f"  error: submit failed (exit {result.returncode})")
        print(f"  stderr: {result.stderr.strip()}", file=sys.stderr)
        return {"slug": slug, "source": source, "status": "error", "reason": result.stderr.strip()}

    # CLI writes "  id: ctoml_...\n  url: ...\n  status: ..." to stderr
    ctoml_id = ctoml_url = submit_status = ""
    for line in result.stderr.splitlines():
        line = line.strip()
        if line.startswith("id: "):
            ctoml_id = line[4:]
        elif line.startswith("url: "):
            ctoml_url = line[5:]
        elif line.startswith("status: "):
            submit_status = line[8:]

    print(f"  id: {ctoml_id}  status: {submit_status}")
    return {
        "slug": slug,
        "source": source,
        "status": "submitted",
        "ctoml_id": ctoml_id,
        "ctoml_url": ctoml_url,
        "submit_status": submit_status,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true", help="Print what would be published without making network calls")
    parser.add_argument("--yes", action="store_true", help="Actually publish to the registry")
    parser.add_argument("--force", action="store_true", help="Re-publish even if an identical candidate already exists")
    args = parser.parse_args()

    if not args.dry_run and not args.yes:
        print("error: pass --dry-run or --yes", file=sys.stderr)
        sys.exit(1)

    samples = load_manifest()
    to_publish = [s for s in samples if s.get("publish_status") == "pending"]
    skipped = [s for s in samples if s.get("publish_status") == "skip"]

    print(f"Manifest: {len(to_publish)} pending, {len(skipped)} skip")
    print(f"API: {API_BASE}")
    if args.dry_run:
        print("Mode: dry-run\n")
    else:
        print("Mode: publish\n")

    results = []
    for sample in to_publish:
        r = publish_sample(sample, dry_run=args.dry_run, force=args.force)
        if r:
            results.append(r)

    for sample in skipped:
        print(f"\n── {sample['slug']} — skip ({sample.get('notes', '')})")

    if args.dry_run:
        print("\nDry-run complete. No changes made.")
        return

    if results:
        out_path = REPO_ROOT / f"docs/aodd/community-publish/published-samples-{date.today().isoformat()}.json"
        with open(out_path, "w") as f:
            json.dump(results, f, indent=2)
        print(f"\nOutput written to {out_path.relative_to(REPO_ROOT)}")

    errors = [r for r in results if r.get("status") == "error"]
    if errors:
        print(f"\n{len(errors)} error(s):", file=sys.stderr)
        for e in errors:
            print(f"  {e['slug']}: {e.get('reason', 'unknown')}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
