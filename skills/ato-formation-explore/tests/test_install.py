"""Filesystem behavior checks; no Coordinator, Runtime, or model is called."""

import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SKILL = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("formation_skill_install",
                                            SKILL / "scripts" / "install.py")
INSTALLER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(INSTALLER)


class SkillInstallationTests(unittest.TestCase):
    def setUp(self):
        test_root = SKILL.parents[1] / ".tmp"
        test_root.mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="skill-install-",
                                                    dir=test_root)
        self.project = Path(self.temporary.name)

    def tearDown(self):
        self.temporary.cleanup()

    def destination(self, agent):
        return (self.project / INSTALLER.LOCATIONS[agent] / "skills"
                / INSTALLER.SKILL_NAME)

    def test_both_products_read_the_same_package_and_reinstall_is_idempotent(self):
        first = INSTALLER.install(self.project, "both")
        self.assertEqual([item["state"] for item in first["locations"]],
                         ["create", "create"])
        targets = []
        for name in INSTALLER.LOCATIONS:
            destination = self.destination(name)
            self.assertTrue(destination.is_symlink())
            self.assertFalse(Path(os.readlink(destination)).is_absolute())
            self.assertEqual(destination.resolve(), SKILL)
            for relative in ["SKILL.md", "references/protocol.md",
                             "scripts/install.py", "agents/openai.yaml"]:
                self.assertTrue((destination / relative).samefile(SKILL / relative))
            targets.append(os.readlink(destination))
        again = INSTALLER.install(self.project, "both")
        self.assertEqual([item["state"] for item in again["locations"]],
                         ["existing", "existing"])
        self.assertEqual(targets, [os.readlink(self.destination(name))
                                  for name in INSTALLER.LOCATIONS])

    def test_existing_skill_rejection_preserves_content_and_creates_no_other_link(self):
        conflict = self.destination("claude-code")
        conflict.mkdir(parents=True)
        marker = conflict / "user-instructions.md"
        marker.write_text("retain me", encoding="utf-8")
        with self.assertRaises(ValueError):
            INSTALLER.install(self.project, "both")
        self.assertEqual(marker.read_text(encoding="utf-8"), "retain me")
        self.assertFalse((self.project / ".agents").exists())

    def test_foreign_and_broken_symlinks_are_not_replaced(self):
        destination = self.destination("codex")
        destination.parent.mkdir(parents=True)
        for target in [self.project, self.project / "missing-target"]:
            destination.symlink_to(target, target_is_directory=True)
            with self.assertRaises(ValueError):
                INSTALLER.install(self.project, "codex")
            self.assertEqual(os.readlink(destination), str(target))
            destination.unlink()

    def test_file_collision_is_not_overwritten(self):
        destination = self.destination("codex")
        destination.parent.mkdir(parents=True)
        destination.write_text("existing file", encoding="utf-8")
        with self.assertRaises(ValueError):
            INSTALLER.install(self.project, "codex")
        self.assertEqual(destination.read_text(encoding="utf-8"), "existing file")

    def test_symlinked_parent_cannot_redirect_installation(self):
        outside = self.project / "unrelated"
        outside.mkdir()
        (self.project / ".agents").symlink_to(outside, target_is_directory=True)
        with self.assertRaises(ValueError):
            INSTALLER.install(self.project, "codex")
        self.assertEqual(list(outside.iterdir()), [])

    def test_write_failure_rolls_back_only_own_links(self):
        original = Path.symlink_to

        def fail_second(destination, target, target_is_directory=False):
            if ".claude" in destination.parts:
                raise OSError("simulated symlink failure")
            return original(destination, target,
                            target_is_directory=target_is_directory)

        with patch.object(Path, "symlink_to", fail_second):
            with self.assertRaises(OSError):
                INSTALLER.install(self.project, "both")
        self.assertFalse((self.project / ".agents").exists())
        self.assertFalse((self.project / ".claude").exists())

    def test_invalid_project_or_package_writes_nothing(self):
        with self.assertRaises(FileNotFoundError):
            INSTALLER.install(self.project / "absent", "both")
        with self.assertRaises(ValueError):
            INSTALLER.install(self.project, "both", source=self.project)
        self.assertEqual(list(self.project.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
