#!/usr/bin/env python3
"""Install one shared Formation skill without replacing an existing skill."""

import argparse
import json
import os
from pathlib import Path


SKILL_NAME = "ato-formation-explore"
LOCATIONS = {"codex": ".agents", "claude-code": ".claude"}


def package_path():
    return Path(__file__).resolve().parents[1]


def check_destination(destination, source):
    for parent in (destination.parent.parent, destination.parent):
        if parent.is_symlink():
            raise ValueError(f"配置先の親がsymlinkです: {parent}")
        if parent.exists() and not parent.is_dir():
            raise ValueError(f"配置先の親がdirectoryではありません: {parent}")
    if destination.is_symlink() and destination.resolve() == source:
        return "existing"
    if os.path.lexists(destination):
        raise ValueError(f"既存のSkillは上書きしません: {destination}")
    return "create"


def install(project, agent, source=None):
    project = Path(project).resolve(strict=True)
    if not project.is_dir():
        raise ValueError("projectは既存directoryを指定してください")
    source = (Path(source) if source else package_path()).resolve(strict=True)
    if not (source / "SKILL.md").is_file():
        raise ValueError("共通Skill packageにSKILL.mdがありません")
    agents = list(LOCATIONS) if agent == "both" else [agent]
    if any(name not in LOCATIONS for name in agents):
        raise ValueError("agentはcodex、claude-code、bothから指定してください")

    # Preflight all locations before writing, so a conflict cannot leave a
    # partially installed pair. Never delete or overwrite an existing entry.
    destinations = []
    for name in agents:
        destination = project / LOCATIONS[name] / "skills" / SKILL_NAME
        state = check_destination(destination, source)
        destinations.append((name, destination, state))

    created_links = []
    created_directories = []
    try:
        for _, destination, state in destinations:
            if state == "existing":
                continue
            for parent in (destination.parent.parent, destination.parent):
                if not parent.exists():
                    parent.mkdir()
                    created_directories.append(parent)
                elif parent.is_symlink() or not parent.is_dir():
                    raise ValueError(f"配置先が変更されました: {parent}")
            # Relative links survive moving a project containing the package.
            destination.symlink_to(os.path.relpath(source, destination.parent),
                                   target_is_directory=True)
            created_links.append(destination)
    except (OSError, ValueError):
        for destination in reversed(created_links):
            destination.unlink()
        for directory in reversed(created_directories):
            try:
                directory.rmdir()
            except OSError:
                pass  # Keep a directory if something else has populated it.
        raise

    return {"skill": SKILL_NAME, "source": str(source),
            "locations": [{"agent": name, "path": str(path), "state": state}
                          for name, path, state in destinations]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", required=True, type=Path)
    parser.add_argument("--agent", required=True,
                        choices=["codex", "claude-code", "both"])
    args = parser.parse_args()
    try:
        result = install(args.project, args.agent)
    except (OSError, ValueError) as error:
        parser.exit(1, f"Skill配置を拒否しました: {error}\n")
    print(json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    main()
