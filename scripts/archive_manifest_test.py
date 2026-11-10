#!/usr/bin/env python3
"""Tests for scripts/archive_manifest.py (run: python3 scripts/archive_manifest_test.py)."""
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from archive_manifest import SCHEMA, build_manifest  # noqa: E402


def git(repo, *args):
    return subprocess.check_output(["git", "-C", str(repo), *args]).decode().strip()


class ArchiveManifest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.repo = Path(self.directory.name)
        git(self.repo, "init", "-q")
        git(self.repo, "config", "user.email", "test@example.invalid")
        git(self.repo, "config", "user.name", "Test")
        archive = self.repo / "docs" / "archive"
        (archive / "deep").mkdir(parents=True)
        (archive / "a.md").write_text("alpha\n", encoding="utf-8")
        (archive / "deep" / "file b.md").write_text("beta\n", encoding="utf-8")
        (archive / "deep" / "image.bin").write_bytes(b"\x00\x01\x02")
        (self.repo / "outside.md").write_text("not archived\n", encoding="utf-8")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "fixture")
        self.commit = git(self.repo, "rev-parse", "HEAD")

    def tearDown(self):
        self.directory.cleanup()

    def build(self, **overrides):
        arguments = dict(
            repo=self.repo,
            ref="HEAD",
            archive_path="docs/archive",
            license_spdx="MIT",
            destination_base="https://example.invalid/ato-archive/tree/main/docs/archive/",
        )
        arguments.update(overrides)
        return build_manifest(**arguments)

    def test_manifest_records_source_commit_digests_and_destination(self):
        manifest = self.build()
        self.assertEqual(manifest["schema"], SCHEMA)
        self.assertEqual(manifest["origin_commit"], self.commit)
        self.assertEqual(manifest["file_count"], 3)
        self.assertGreater(manifest["total_bytes"], 0)
        entry = next(entry for entry in manifest["entries"] if entry["path"] == "a.md")
        self.assertEqual(entry["sha256"], "sha256" and __import__("hashlib").sha256(b"alpha\n").hexdigest())
        self.assertEqual(entry["license"], "MIT")
        self.assertEqual(entry["destination_url"], "https://example.invalid/ato-archive/tree/main/docs/archive/a.md")
        escaped = next(entry for entry in manifest["entries"] if entry["path"] == "deep/file b.md")
        self.assertTrue(escaped["destination_url"].endswith("deep/file%20b.md"))
        self.assertEqual(manifest["entries_digest"], "sha256:" + manifest["entries_digest"].split(":", 1)[1])
        self.assertEqual(len(manifest["entries_digest"]), len("sha256:") + 64)

    def test_the_manifest_is_deterministic_and_sorted(self):
        first = self.build()
        second = self.build()
        self.assertEqual(first["entries_digest"], second["entries_digest"])
        self.assertEqual(
            [entry["path"] for entry in first["entries"]],
            sorted(entry["path"] for entry in first["entries"]),
        )

    def test_a_changed_file_changes_the_entries_digest(self):
        before = self.build()
        (self.repo / "docs" / "archive" / "a.md").write_text("changed\n", encoding="utf-8")
        git(self.repo, "commit", "-q", "-am", "change")
        after = self.build()
        self.assertNotEqual(before["entries_digest"], after["entries_digest"])
        changed = next(entry for entry in after["entries"] if entry["path"] == "a.md")
        self.assertEqual(changed["sha256"], __import__("hashlib").sha256(b"changed\n").hexdigest())

    def test_only_the_archive_tree_is_included(self):
        manifest = self.build()
        self.assertNotIn("outside.md", [entry["path"] for entry in manifest["entries"]])

    def test_the_cli_writes_one_json_object(self):
        output = self.repo / "manifest.json"
        subprocess.check_call(
            [
                sys.executable,
                str(Path(__file__).resolve().parent / "archive_manifest.py"),
                "--repo",
                str(self.repo),
                "--archive-path",
                "docs/archive",
                "--license",
                "MIT",
                "--destination-base",
                "https://example.invalid/archive",
                "--output",
                str(output),
            ]
        )
        manifest = json.loads(output.read_text(encoding="utf-8"))
        self.assertEqual(manifest["origin_commit"], self.commit)


if __name__ == "__main__":
    unittest.main()
