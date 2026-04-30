#!/usr/bin/env bash
set -euo pipefail

latest_tag="$(git tag --list 'v[0-9]*' --sort=-v:refname | head -n 1)"
if [[ -z "${latest_tag}" ]]; then
  echo "No previous release tag found; release-plz will treat this as an initial release."
  exit 0
fi

base_commit="$(git rev-list -n 1 "${latest_tag}")"
worktree_dir=".tmp/release-plz-previous-tag"

rm -rf "${worktree_dir}"
git worktree add --detach "${worktree_dir}" "${base_commit}"
trap 'git worktree remove --force "${worktree_dir}" >/dev/null 2>&1 || true' EXIT

manifest="${worktree_dir}/crates/ato-desktop/Cargo.toml"
workspace_manifest="${worktree_dir}/Cargo.toml"
perl -0pi -e 's/members = \[\n.*?\n\]/members = [\n    "crates\/ato-cli",\n    "crates\/capsule-core",\n    "crates\/capsule-wire",\n    "crates\/ato-cli\/lock-draft-engine",\n    "crates\/ato-session-core",\n\]/s' "${workspace_manifest}"

if [[ ! -f "${manifest}" ]]; then
  echo "Previous tag ${latest_tag} has no ato-desktop manifest; no compatibility patch needed."
  exit 0
fi

perl -0pi -e '
  s/ato-session-core = \{ path = "\.\.\/ato-session-core" \}/ato-session-core = { path = "..\/ato-session-core", version = "0.1.0" }/g;
  s/capsule-core = \{ path = "\.\.\/capsule-core" \}/capsule-core = { path = "..\/capsule-core", version = "0.11.0" }/g;
  s/gpui = \{ git = "https:\/\/github\.com\/zed-industries\/zed", rev = "15d86607" \}/gpui = { version = "0.2.2", git = "https:\/\/github.com\/zed-industries\/zed", rev = "15d86607" }/g;
  s/gpui_platform = \{ git = "https:\/\/github\.com\/zed-industries\/zed", rev = "15d86607", features = \["font-kit", "x11", "wayland", "runtime_shaders"\] \}/gpui_platform = { version = "0.1.0", git = "https:\/\/github.com\/zed-industries\/zed", rev = "15d86607", features = ["font-kit", "x11", "wayland", "runtime_shaders"] }/g;
  s/gpui-component = \{ git = "https:\/\/github\.com\/ato-run\/gpui-component", rev = "00ea23fbf0d15e229c9ec3034b2c3c16846908f9" \}/gpui-component = { version = "0.5.1", git = "https:\/\/github.com\/ato-run\/gpui-component", rev = "00ea23fbf0d15e229c9ec3034b2c3c16846908f9" }/g;
  s/gpui-component-assets = \{ git = "https:\/\/github\.com\/ato-run\/gpui-component", rev = "00ea23fbf0d15e229c9ec3034b2c3c16846908f9" \}/gpui-component-assets = { version = "0.5.1", git = "https:\/\/github.com\/ato-run\/gpui-component", rev = "00ea23fbf0d15e229c9ec3034b2c3c16846908f9" }/g;
' "${manifest}"

if git -C "${worktree_dir}" diff --quiet -- Cargo.toml crates/ato-desktop/Cargo.toml; then
  echo "Previous tag ${latest_tag} already has package-compatible ato-desktop dependencies."
  exit 0
fi

git -C "${worktree_dir}" add Cargo.toml crates/ato-desktop/Cargo.toml
git -C "${worktree_dir}" \
  -c user.name="ato release automation" \
  -c user.email="release@ato.run" \
  commit -m "ci: make previous release tag package-compatible for release-plz"

patched_commit="$(git -C "${worktree_dir}" rev-parse HEAD)"
git tag --force "${latest_tag}" "${patched_commit}"
echo "Locally retargeted ${latest_tag} to ${patched_commit} for release-plz package comparison."
