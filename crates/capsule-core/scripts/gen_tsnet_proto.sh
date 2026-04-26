#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
CORE_DIR=$(cd "${SCRIPT_DIR}/.." && pwd)
PROTO_FILE="${CORE_DIR}/proto/tsnet/v1/tsnet.proto"
OUT_FILE="${CORE_DIR}/src/tsnet/tsnet.v1.rs"

if [[ ! -f "${PROTO_FILE}" ]]; then
  echo "proto file not found: ${PROTO_FILE}" >&2
  exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo is required" >&2
  exit 1
fi

TMP_DIR=$(mktemp -d)
cleanup() {
  rm -rf "${TMP_DIR}"
}
trap cleanup EXIT

cat > "${TMP_DIR}/Cargo.toml" <<'CARGO_TOML'
[package]
name = "tsnet-proto-gen"
version = "0.1.0"
edition = "2021"

[dependencies]
tonic-build = "0.12"
CARGO_TOML

mkdir -p "${TMP_DIR}/src"
cat > "${TMP_DIR}/src/main.rs" <<'RS'
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let core_dir = std::env::args().nth(1).expect("core_dir is required");
    let proto_file = format!("{core_dir}/proto/tsnet/v1/tsnet.proto");
    let include_dir = format!("{core_dir}/proto");
    let out_dir = format!("{core_dir}/src/tsnet");

    tonic_build::configure()
        .build_client(true)
        .build_server(false)
        .out_dir(out_dir)
        .compile_protos(&[proto_file], &[include_dir])?;

    Ok(())
}
RS

cargo run --quiet --manifest-path "${TMP_DIR}/Cargo.toml" -- "${CORE_DIR}"

echo "updated: ${OUT_FILE}"
