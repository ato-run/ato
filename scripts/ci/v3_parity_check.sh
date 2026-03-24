#!/usr/bin/env bash
set -euo pipefail

fail() {
  echo "ERROR: $*" >&2
  exit 1
}

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ATO_BIN="$ROOT/target/debug/ato"
[[ -x "$ATO_BIN" ]] || fail "ato binary not found: $ATO_BIN"

SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-1704067200}"
WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

PROJECT_DIR="$WORKDIR/project"
mkdir -p "$PROJECT_DIR/assets"

cat > "$PROJECT_DIR/capsule.toml" <<'TOML'
schema_version = "0.2"
name = "v3-parity-fixture"
version = "1.0.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
entrypoint = "run.sh"
TOML

cat > "$PROJECT_DIR/run.sh" <<'SH'
#!/usr/bin/env sh
cat ./assets/message.txt
SH
chmod +x "$PROJECT_DIR/run.sh"
echo "v3 parity fixture" > "$PROJECT_DIR/assets/message.txt"

# Normalize file timestamps so artifact output is stable in CI.
if touch -h -d "@$SOURCE_DATE_EPOCH" "$PROJECT_DIR/capsule.toml" 2>/dev/null; then
  find "$PROJECT_DIR" -print0 | xargs -0 touch -h -d "@$SOURCE_DATE_EPOCH"
else
  BSD_TOUCH_TS="$(date -u -r "$SOURCE_DATE_EPOCH" +%Y%m%d%H%M.%S)"
  find "$PROJECT_DIR" -print0 | xargs -0 touch -h -t "$BSD_TOUCH_TS"
fi

HOME_A="$WORKDIR/home-a"
HOME_B="$WORKDIR/home-b"
HOME_OPEN="$WORKDIR/home-open"
CAS_BUILD_A="$WORKDIR/cas-build-a"
CAS_BUILD_B="$WORKDIR/cas-build-b"
CAS_OPEN_STRICT="$CAS_BUILD_B"
mkdir -p "$HOME_A" "$HOME_B" "$HOME_OPEN" "$CAS_BUILD_A" "$CAS_BUILD_B"

echo "[1/6] Build artifact with ATO_EXPERIMENTAL_V3_PACK=0"
(
  cd "$PROJECT_DIR"
  HOME="$HOME_A" \
  ATO_CAS_ROOT="$CAS_BUILD_A" \
  ATO_EXPERIMENTAL_V3_PACK=0 \
  "$ATO_BIN" build .
)
ARTIFACT_A="$WORKDIR/artifact-flag0.capsule"
cp "$PROJECT_DIR/v3-parity-fixture.capsule" "$ARTIFACT_A"

echo "[2/6] Build artifact with ATO_EXPERIMENTAL_V3_PACK=1"
(
  cd "$PROJECT_DIR"
  HOME="$HOME_B" \
  ATO_CAS_ROOT="$CAS_BUILD_B" \
  ATO_EXPERIMENTAL_V3_PACK=1 \
  "$ATO_BIN" build .
)
ARTIFACT_B="$WORKDIR/artifact-flag1.capsule"
cp "$PROJECT_DIR/v3-parity-fixture.capsule" "$ARTIFACT_B"

mkdir -p "$WORKDIR/ext-a" "$WORKDIR/ext-b"
tar -xf "$ARTIFACT_A" -C "$WORKDIR/ext-a"
tar -xf "$ARTIFACT_B" -C "$WORKDIR/ext-b"

echo "[3/6] Verify v3 manifest presence/absence"
if tar -tf "$ARTIFACT_A" | grep -q '^payload\.v3\.manifest\.json$'; then
  fail "flag=0 artifact unexpectedly contains payload.v3.manifest.json"
fi
if ! tar -tf "$ARTIFACT_B" | grep -q '^payload\.v3\.manifest\.json$'; then
  fail "flag=1 artifact must contain payload.v3.manifest.json"
fi

echo "[4/6] Strict diff: payload.tar.zst SHA256 must match exactly"
SHA_A="$(sha256sum "$WORKDIR/ext-a/payload.tar.zst" | awk '{print $1}')"
SHA_B="$(sha256sum "$WORKDIR/ext-b/payload.tar.zst" | awk '{print $1}')"
if [[ "$SHA_A" != "$SHA_B" ]]; then
  fail "payload.tar.zst sha256 mismatch: flag0=$SHA_A flag1=$SHA_B"
fi

open_capsule() {
  local artifact="$1"
  local output_root="$2"
  local cas_root="$3"
  local flag="$4"
  local local_artifact

  mkdir -p "$output_root"
  local_artifact="$output_root/$(basename "$artifact")"
  cp "$artifact" "$local_artifact"
  (
    cd "$output_root"
    HOME="$HOME_OPEN" \
    ATO_CAS_ROOT="$cas_root" \
    ATO_EXPERIMENTAL_V3_PACK="$flag" \
    CAPSULE_ALLOW_UNSAFE=1 \
    "$ATO_BIN" run "$local_artifact" --dangerously-skip-permissions --yes >/dev/null
  )
}

manifest_payload_tree() {
  local extracted_dir="$1"
  local output_file="$2"

  file_mode() {
    local path="$1"
    if stat -c '%a' "$path" >/dev/null 2>&1; then
      stat -c '%a' "$path"
    else
      stat -f '%Lp' "$path"
    fi
  }

  (
    cd "$extracted_dir"
    find . -type f \
      ! -name 'capsule.toml' \
      ! -name 'capsule.lock' \
      ! -name 'signature.json' \
      ! -name 'sbom.spdx.json' \
      ! -name 'payload.v3.manifest.json' \
      -print0 \
    | sort -z \
    | while IFS= read -r -d '' path; do
        mode="$(file_mode "$path")"
        sha="$(sha256sum "$path" | awk '{print $1}')"
        printf '%s %s %s\n' "$sha" "$mode" "$path"
      done
  ) > "$output_file"
}

echo "[5/6] Strict diff: extracted payload trees must match"
OPEN_A="$WORKDIR/open-a"
OPEN_B="$WORKDIR/open-b"
open_capsule "$ARTIFACT_A" "$OPEN_A" "$CAS_OPEN_STRICT" "1"
open_capsule "$ARTIFACT_B" "$OPEN_B" "$CAS_OPEN_STRICT" "1"

EXTRACTED_A="$(find "$OPEN_A" -maxdepth 1 -type d -name '*-extracted' | head -n1)"
EXTRACTED_B="$(find "$OPEN_B" -maxdepth 1 -type d -name '*-extracted' | head -n1)"
[[ -n "$EXTRACTED_A" ]] || fail "flag=0 artifact open did not produce extracted dir"
[[ -n "$EXTRACTED_B" ]] || fail "flag=1 artifact open did not produce extracted dir"

TREE_A="$WORKDIR/tree-a.manifest"
TREE_B="$WORKDIR/tree-b.manifest"
manifest_payload_tree "$EXTRACTED_A" "$TREE_A"
manifest_payload_tree "$EXTRACTED_B" "$TREE_B"

if ! diff -u "$TREE_A" "$TREE_B" >/dev/null; then
  echo "Extracted tree mismatch (payload files):"
  diff -u "$TREE_A" "$TREE_B" || true
  fail "extracted payload trees are not identical"
fi

echo "[6/6] Cross-compatibility checks"
# Backward compatibility: flag=1 artifact must open with legacy mode (CAS disabled).
LEGACY_OPEN="$WORKDIR/open-legacy"
open_capsule "$ARTIFACT_B" "$LEGACY_OPEN" "/dev/null/ato-cas" "0"
LEGACY_EXTRACTED="$(find "$LEGACY_OPEN" -maxdepth 1 -type d -name '*-extracted' | head -n1)"
[[ -n "$LEGACY_EXTRACTED" ]] || fail "legacy-mode open for flag=1 artifact failed"

# Forward compatibility: flag=0 artifact must open in enhanced mode (CAS enabled).
ENHANCED_OPEN="$WORKDIR/open-enhanced"
open_capsule "$ARTIFACT_A" "$ENHANCED_OPEN" "$WORKDIR/cas-open-enhanced" "1"
ENHANCED_EXTRACTED="$(find "$ENHANCED_OPEN" -maxdepth 1 -type d -name '*-extracted' | head -n1)"
[[ -n "$ENHANCED_EXTRACTED" ]] || fail "enhanced-mode open for flag=0 artifact failed"

echo "PASS: v3 dual-build parity and compatibility checks succeeded"
