#!/usr/bin/env python3
"""Build the snapshot manifest for moving an archive tree to another Git repo.

The manifest is the evidence a reader needs to trust a moved archive snapshot:
the exact source commit, the blob id and content digest of every file, its size,
its licence position and where it now lives. It is deterministic: the same
source tree produces the same manifest and the same digest.

This script is read-only. It reads `git ls-tree` / `git show` from the source
repository and writes one JSON object; it never moves, deletes or rewrites a
file.
"""
import argparse
import hashlib
import json
import mimetypes
import subprocess
import sys
from pathlib import Path
from urllib.parse import quote

SCHEMA = "ato.archive-manifest/1"


def _git(repo, *args):
    return subprocess.check_output(["git", "-C", str(repo), *args])


def _tree_entries(repo, ref, archive_path):
    """Yield (path relative to archive_path, blob sha1, declared size)."""
    output = _git(repo, "ls-tree", "-r", "-l", "-z", "--full-tree", ref, "--", archive_path)
    for record in output.decode("utf-8", "surrogateescape").split("\0"):
        if not record:
            continue
        meta, full_path = record.split("\t", 1)
        _mode, kind, blob, size = meta.split()
        if kind != "blob":
            continue
        if not full_path.startswith(archive_path.rstrip("/") + "/"):
            continue
        yield full_path[len(archive_path.rstrip("/")) + 1:], blob, size


def _canonical_digest(entries):
    canonical = json.dumps(entries, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
    return "sha256:" + hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def build_manifest(repo, ref, archive_path, license_spdx, destination_base, origin_commit=None):
    """Return the manifest object for `archive_path` at `ref`."""
    repo = Path(repo)
    resolved_commit = origin_commit or _git(repo, "rev-parse", "--verify", f"{ref}^{{commit}}").decode().strip()
    entries = []
    for relative, blob, declared_size in _tree_entries(repo, ref, archive_path):
        content = _git(repo, "show", f"{ref}:{archive_path.rstrip('/')}/{relative}")
        media_type = mimetypes.guess_type(relative)[0] or "application/octet-stream"
        entries.append({
            "path": relative,
            "bytes": len(content),
            "declared_size": None if declared_size == "-" else int(declared_size),
            "git_blob_sha1": blob,
            "sha256": hashlib.sha256(content).hexdigest(),
            "media_type": media_type,
            "license": license_spdx,
            "destination_url": destination_base.rstrip("/") + "/" + quote(relative, safe="/"),
        })
    entries.sort(key=lambda entry: entry["path"])
    return {
        "schema": SCHEMA,
        "source_repo": str(repo),
        "origin_commit": resolved_commit,
        "archive_path": archive_path.rstrip("/"),
        "license": license_spdx,
        "destination_base": destination_base.rstrip("/"),
        "file_count": len(entries),
        "total_bytes": sum(entry["bytes"] for entry in entries),
        "entries_digest": _canonical_digest(entries),
        "entries": entries,
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True, help="Source checkout")
    parser.add_argument("--ref", default="HEAD", help="Snapshot to describe (default: HEAD)")
    parser.add_argument("--origin-commit", help="Record this commit instead of resolving --ref")
    parser.add_argument("--archive-path", required=True, help="Tree to move, e.g. docs/archive")
    parser.add_argument("--license", required=True, help="SPDX licence of the moved files")
    parser.add_argument("--destination-base", required=True, help="URL prefix in the archive repo")
    parser.add_argument("--output", default="-", help="Write the manifest here ('-' for stdout)")
    args = parser.parse_args(argv)
    manifest = build_manifest(
        args.repo, args.ref, args.archive_path, args.license, args.destination_base, args.origin_commit
    )
    text = json.dumps(manifest, indent=2, sort_keys=True) + "\n"
    if args.output == "-":
        sys.stdout.write(text)
    else:
        Path(args.output).write_text(text, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
