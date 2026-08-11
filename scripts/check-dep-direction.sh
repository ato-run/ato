#!/usr/bin/env bash
#
# check-dep-direction.sh — enforce the workspace dependency DAG.
#
# Parses each crate's Cargo.toml dependency tables and fails (exit non-zero)
# if any crate depends on a workspace crate it is forbidden from depending on.
# This keeps the wire / domain / runtime / shell layering honest:
#
#   protocol / capsule-protocol — DAG roots: NO workspace-crate deps.
#   capsule       — domain + local state: may dep protocol; NOT
#                   cli / desktop / netd / nacelle.
#   netd          — speaks protocol: may dep protocol; NOT
#                   capsule / cli / desktop / nacelle.
#   nacelle       — sandbox only: may dep protocol; NOT
#                   capsule / cli / desktop / netd.
#   cli           — orchestrator: may dep capsule / protocol / nacelle;
#                   NOT desktop.
#   desktop       — shell: may dep protocol / capsule; NOT
#                   cli / netd / nacelle. Spawns the `ato` binary
#                   as a subprocess instead of linking cli.
#
# It also fails if any of the pre-reorg package names are still present as a
# crate anywhere (capsule-wire, capsule-core, ato-session-core, ato-net,
# ato-cli, ato-desktop, ato-netd, ato-protocol).
#
# Note: `lock-draft-engine` (a shared helper under crates/cli/) is an
# allowed dependency for any crate — it is intentionally not part of the
# forbidden lists.
#
# Run locally with: bash scripts/check-dep-direction.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

fail=0

err() {
  printf '  ✗ %s\n' "$1" >&2
  fail=1
}

# Extract workspace-crate dependency names from a Cargo.toml.
#
# Walks every dependency table ([dependencies], [dev-dependencies],
# [build-dependencies], and target-specific `[target.'...'.dependencies]`),
# ignoring comment lines, and prints the dependency key (the bare crate name
# before `=`) one per line. Only keys naming a known workspace crate (or a
# banned legacy crate) matter to the caller; the caller filters.
crate_deps() {
  local manifest="$1"
  awk '
    # Track whether the current section is a dependency table.
    /^[[:space:]]*\[/ {
      line = $0
      sub(/[[:space:]]*#.*/, "", line)          # strip trailing comment
      if (line ~ /dependencies\][[:space:]]*$/ || line ~ /dependencies\.[^]]+\][[:space:]]*$/) {
        indep = 1
      } else {
        indep = 0
      }
      next
    }
    indep {
      line = $0
      sub(/#.*/, "", line)                       # drop comments
      gsub(/^[[:space:]]+/, "", line)            # left-trim
      if (line == "") next
      # A dependency line looks like:  name = "..."  or  name = { ... }
      # We also accept dotted keys like  name.workspace = true.
      if (match(line, /^[A-Za-z0-9_-]+/)) {
        key = substr(line, RSTART, RLENGTH)
        rest = substr(line, RSTART + RLENGTH)
        gsub(/^[[:space:]]+/, "", rest)
        # Must be an assignment (key = ... or key.field = ...).
        if (rest ~ /^=/ || rest ~ /^\./) {
          print key
        }
      }
    }
  ' "$manifest" | sort -u
}

# has_dep <manifest> <crate-name> -> 0 if present, 1 otherwise.
has_dep() {
  crate_deps "$1" | grep -qx "$2"
}

# check_forbidden <crate-label> <manifest> <forbidden...>
check_forbidden() {
  local label="$1" manifest="$2"
  shift 2
  if [ ! -f "$manifest" ]; then
    err "$label: manifest not found at $manifest"
    return
  fi
  local dep
  for dep in "$@"; do
    if has_dep "$manifest" "$dep"; then
      err "$label must NOT depend on '$dep' (found in $manifest)"
    fi
  done
}

# Read the package set from Cargo rather than maintaining another partial list.
# `--no-deps` keeps this local and deterministic; package names are sufficient
# because dependency keys in this workspace use package names directly.
WORKSPACE_PACKAGES=()
while IFS= read -r package; do
  WORKSPACE_PACKAGES+=("$package")
done < <(
  cargo metadata --format-version 1 --no-deps \
    | jq -r '.workspace_members as $members | .packages[] | select(.id as $id | $members | index($id)) | .name' \
    | sort -u
)

check_dag_root() {
  local root="$1" manifest="$2"
  local package
  for package in "${WORKSPACE_PACKAGES[@]}"; do
    if [ "$package" != "$root" ] && has_dep "$manifest" "$package"; then
      err "$root is a DAG root and must NOT depend on workspace package '$package'"
    fi
  done
}

echo "Checking workspace dependency direction..."

# Semantic and IPC roots may not depend on any other workspace package.
check_dag_root "protocol" "crates/protocol/Cargo.toml"
check_dag_root "capsule-protocol" "crates/capsule-protocol/Cargo.toml"

# capsule: may dep protocol; not the rest.
check_forbidden "capsule" "crates/capsule/Cargo.toml" \
  cli desktop netd nacelle

# netd: may dep protocol; not the rest.
check_forbidden "netd" "crates/netd/Cargo.toml" \
  capsule cli desktop nacelle

# nacelle: may dep protocol; not the rest.
check_forbidden "nacelle" "crates/nacelle/Cargo.toml" \
  capsule cli desktop netd

# cli: may dep capsule / protocol / nacelle; not desktop.
check_forbidden "cli" "crates/cli/Cargo.toml" \
  desktop

# desktop: may dep protocol / capsule; not the rest.
check_forbidden "desktop" "crates/desktop/Cargo.toml" \
  cli netd nacelle

# Banned legacy package names: must not appear as a crate dependency or as a
# crate directory anywhere in the workspace.
echo "Checking for banned legacy crate names..."
BANNED=(capsule-wire capsule-core ato-session-core ato-net ato-cli ato-desktop ato-netd ato-protocol)
for name in "${BANNED[@]}"; do
  # As a crate directory.
  if [ -d "crates/$name" ]; then
    err "banned legacy crate directory still exists: crates/$name"
  fi
  # As a dependency key in any crate manifest.
  while IFS= read -r manifest; do
    if crate_deps "$manifest" | grep -qx "$name"; then
      err "banned legacy crate '$name' is still a dependency in $manifest"
    fi
  done < <(find crates -name Cargo.toml -not -path '*/target/*')
done

if [ "$fail" -ne 0 ]; then
  echo >&2
  echo "Dependency-direction check FAILED." >&2
  exit 1
fi

echo "OK: dependency direction is sound; no banned legacy crates."
