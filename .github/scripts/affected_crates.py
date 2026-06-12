#!/usr/bin/env python3
"""Select the workspace crates whose tests are relevant to a change set.

Reads changed file paths (repo-relative, one per line) on stdin and prints the
crates that should be tested, one `cargo -p` package name per line, on stdout.

Selection rule: a crate is *affected* if it was changed directly, or if it
transitively depends on a changed crate (so a fix to a low-level crate still
exercises every dependent's tests). `ato-desktop` is excluded from the cargo
workspace entirely (own Cargo.lock, GUI-only git deps) and is never built or
tested in CI, so changes confined to it select nothing.

Failure/uncertainty is fail-safe: an empty or unreadable diff, or a change to a
build-wide file (root manifest, lockfile, toolchain, this script, the CI
workflow), selects the full testable set.

Empty stdout means "no testable crate is affected — skip the test run".
"""

import json
import subprocess
import sys

# Crates that are cargo-workspace members but must never be tested in CI.
#   - ato-desktop / xtask: outside the cargo workspace (GUI git deps).
#   - nacelle / ato-net / ato-netd: their integration tests (e.g. nacelle's
#     terminal_e2e PTY tests) are not stable on headless CI runners and were
#     deliberately never in the CI test list; keep that policy. A change to one
#     of these still tests any *dependent* in the proven set (e.g. an ato-net
#     change runs ato-cli's tests because ato-cli depends on ato-net).
EXCLUDE = {
    "ato-desktop",
    "ato-desktop-xtask",
    "nacelle",
    "ato-net",
    "ato-netd",
}

# A change touching any of these forces the full testable set, because it can
# affect every crate's build or the selection logic itself.
FULL_TRIGGER_EXACT = {
    "Cargo.toml",
    "Cargo.lock",
    ".github/workflows/rust-ci.yml",
    ".github/scripts/affected_crates.py",
}
FULL_TRIGGER_PREFIX = ("rust-toolchain", ".cargo/")

# Sentinel a caller may emit on stdin when it could not compute a diff.
FULL_SENTINEL = "<FULL>"


def load_workspace():
    """Return (members, dep_edges, member_dirs) from cargo metadata.

    members:      list of workspace member package names
    dep_edges:    {pkg: set(member deps of pkg)}
    member_dirs:  list of (dir, name) sorted longest-dir-first for prefix match
    """
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    meta = json.loads(out)
    root = meta["workspace_root"].replace("\\", "/").rstrip("/")
    members = []
    member_dirs = []
    dep_edges = {}
    raw_deps = {}
    for pkg in meta["packages"]:
        name = pkg["name"]
        members.append(name)
        manifest = pkg["manifest_path"].replace("\\", "/")
        # Directory of the crate, relative to the workspace root.
        d = manifest.rsplit("/", 1)[0]
        rel = d[len(root) + 1 :] if d.startswith(root + "/") else d
        member_dirs.append((rel, name))
        raw_deps[name] = {dep["name"] for dep in pkg.get("dependencies", [])}
    member_set = set(members)
    for name, deps in raw_deps.items():
        dep_edges[name] = {d for d in deps if d in member_set}
    # Longest directory first so nested crates (e.g. crates/ato-cli/
    # lock-draft-engine) win over their parent (crates/ato-cli).
    member_dirs.sort(key=lambda t: len(t[0]), reverse=True)
    return members, dep_edges, member_dirs


def reverse_closure(seed, dep_edges):
    """All crates that transitively depend on any crate in `seed` (plus seed)."""
    # Build reverse adjacency: who depends on X.
    rdeps = {}
    for pkg, deps in dep_edges.items():
        for d in deps:
            rdeps.setdefault(d, set()).add(pkg)
    affected = set(seed)
    stack = list(seed)
    while stack:
        cur = stack.pop()
        for dependent in rdeps.get(cur, ()):
            if dependent not in affected:
                affected.add(dependent)
                stack.append(dependent)
    return affected


def map_to_member(path, member_dirs):
    """Return the member that owns `path`, or None."""
    p = path.replace("\\", "/").strip()
    if not p:
        return None
    for d, name in member_dirs:  # already longest-first
        if d and (p == d or p.startswith(d + "/")):
            return name
    return None


def main():
    changed = [ln.strip() for ln in sys.stdin if ln.strip()]
    members, dep_edges, member_dirs = load_workspace()
    testable = [m for m in members if m not in EXCLUDE]

    def emit(names):
        for n in sorted(names):
            print(n)

    # Fail-safe: no diff information -> run everything.
    if not changed or any(c == FULL_SENTINEL for c in changed):
        print("affected: full set (no/unknown diff)", file=sys.stderr)
        emit(testable)
        return

    if any(
        c in FULL_TRIGGER_EXACT or c.startswith(FULL_TRIGGER_PREFIX) for c in changed
    ):
        print("affected: full set (build-wide file changed)", file=sys.stderr)
        emit(testable)
        return

    changed_members = set()
    for c in changed:
        m = map_to_member(c, member_dirs)
        if m is not None:
            changed_members.add(m)

    if not changed_members:
        print(
            "affected: none (no workspace crate touched; tests skipped)",
            file=sys.stderr,
        )
        return

    affected = reverse_closure(changed_members, dep_edges)
    selected = sorted(a for a in affected if a not in EXCLUDE)
    print(
        f"affected: {', '.join(selected) or 'none'} "
        f"(changed crates: {', '.join(sorted(changed_members))})",
        file=sys.stderr,
    )
    emit(selected)


if __name__ == "__main__":
    main()
