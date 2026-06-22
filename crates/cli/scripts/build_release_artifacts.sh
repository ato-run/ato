#!/usr/bin/env bash
set -euo pipefail

# Build and package ato-cli release archives for one or more Rust targets.
#
# Example:
#   TARGETS="aarch64-apple-darwin" ./scripts/build_release_artifacts.sh
#   TARGETS="x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu" ./scripts/build_release_artifacts.sh
#
# Env:
#   VERSION      default: parsed from Cargo.toml ([package].version)
#   TARGETS      default: host target only
#   OUT_ROOT     default: /tmp/ato-release
#   SKIP_BUILD   default: 0 (set 1 to only package already-built binaries)

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: required command not found: $1" >&2
    exit 1
  }
}

has_cmd() {
  command -v "$1" >/dev/null 2>&1
}

detect_host_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$arch" in
    x86_64|amd64) arch="x86_64" ;;
    arm64|aarch64) arch="aarch64" ;;
    *)
      echo "error: unsupported host architecture: $arch" >&2
      exit 1
      ;;
  esac

  case "$os" in
    Darwin) echo "${arch}-apple-darwin" ;;
    Linux) echo "${arch}-unknown-linux-gnu" ;;
    *)
      echo "error: unsupported host OS: $os" >&2
      exit 1
      ;;
  esac
}

extract_version() {
  local cargo_toml="$1"
  awk '
    /^\[package\]/ { in_package=1; next }
    in_package && /^\[/ { in_package=0 }
    in_package && $1 == "version" {
      gsub(/"/, "", $3)
      print $3
      exit
    }
  ' "$cargo_toml"
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ATO_CLI_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

need_cmd cargo
need_cmd rustup
need_cmd tar
need_cmd zip
need_cmd shasum
need_cmd mktemp

VERSION="${VERSION:-$(extract_version "$ATO_CLI_DIR/Cargo.toml")}"
if [[ -z "$VERSION" ]]; then
  echo "error: failed to detect VERSION from Cargo.toml" >&2
  exit 1
fi

TARGETS="${TARGETS:-$(detect_host_target)}"
HOST_TARGET="$(detect_host_target)"
OUT_ROOT="${OUT_ROOT:-/tmp/ato-release}"
SKIP_BUILD="${SKIP_BUILD:-0}"
OUT_DIR="${OUT_ROOT%/}/${VERSION}"

read -r -a target_list <<<"$TARGETS"
if [[ "${#target_list[@]}" -eq 0 ]]; then
  echo "error: TARGETS is empty" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"

for target in "${target_list[@]}"; do
  echo "==> target: $target"

  if [[ "$SKIP_BUILD" != "1" ]]; then
    rustup target add "$target"

    # On macOS hosts, GNU/Linux cross builds often fail without a target GCC.
    # If available, use cargo-zigbuild to provide a cross toolchain via Zig.
    if [[ "$(uname -s)" == "Darwin" && "$target" == *"-unknown-linux-gnu" ]]; then
      if has_cmd cargo-zigbuild && has_cmd zig; then
        cargo zigbuild --release --locked --target "$target" --manifest-path "$ATO_CLI_DIR/Cargo.toml"
      else
        echo "error: missing cross toolchain for $target" >&2
        echo "hint: install 'cargo-zigbuild' and 'zig', or install ${target%%-*}-linux-gnu-gcc" >&2
        exit 1
      fi
    else
      cargo build --release --locked --target "$target" --manifest-path "$ATO_CLI_DIR/Cargo.toml"
    fi
  fi

  binary_name="ato"
  if [[ "$target" == *"-windows-"* ]]; then
    binary_name="ato.exe"
  fi

  binary_path="$ATO_CLI_DIR/target/$target/release/$binary_name"
  if [[ ! -f "$binary_path" ]]; then
    echo "error: built binary not found: $binary_path" >&2
    exit 1
  fi

  if [[ "$target" == "$HOST_TARGET" ]]; then
    "$binary_path" --version >/dev/null
  fi

  staging_dir="$(mktemp -d)"
  cp "$binary_path" "$staging_dir/$binary_name"
  chmod 0755 "$staging_dir/$binary_name"

  if [[ "$target" == *"-windows-"* ]]; then
    archive_path="$OUT_DIR/ato-cli-$target.zip"
    (
      cd "$staging_dir"
      zip -q "$archive_path" "$binary_name"
    )
  else
    archive_path="$OUT_DIR/ato-cli-$target.tar.xz"
    tar -C "$staging_dir" -cJf "$archive_path" "$binary_name"
  fi
  rm -rf "$staging_dir"

  echo "    packaged: $archive_path"
done

(
  cd "$OUT_DIR"
  shopt -s nullglob
  archives=(ato-cli-*.tar.xz ato-cli-*.zip)
  if [[ "${#archives[@]}" -eq 0 ]]; then
    echo "error: no packaged archives found in $OUT_DIR" >&2
    exit 1
  fi
  shasum -a 256 "${archives[@]}" > SHA256SUMS
)

echo "==> done"
echo "    version : $VERSION"
echo "    out dir : $OUT_DIR"
echo "    targets : ${target_list[*]}"
echo "    checksum: $OUT_DIR/SHA256SUMS"
