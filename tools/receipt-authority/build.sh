#!/bin/sh
# Run from this checkout; output is stable for the pinned toolchain and lockfile.
set -eu
cd "$(dirname "$0")/../.."
export SOURCE_DATE_EPOCH=0
export RUSTFLAGS="--remap-path-prefix=$PWD=/ato -C link-arg=--max-memory=67108864"
cargo build --locked -p ato-receipt-authority --target wasm32-unknown-unknown --profile receipt-wasm
