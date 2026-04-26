# wasm-hello

Minimal Wasm capsule using the `wasmtime` driver.

## What it demonstrates

- `runtime = "wasm"` target kind
- `driver = "wasmtime"` executor selection
- Pre-built `.wasm` artifact tracked in the capsule directory
- Wasm runtime resolves component path from `run_command` when routing via lock

## Run

```bash
ato run .
```

Expected output:

```
hello from wasm capsule
```

## How it was built

The Wasm binary was compiled from `src/main.rs`:

```bash
rustup target add wasm32-wasip1
cargo build --target wasm32-wasip1 --release
cp target/wasm32-wasip1/release/wasm_hello.wasm hello.wasm
```

## Requirements

- `wasmtime` must be in `PATH` (install: <https://wasmtime.dev>)
- `ato` v0.4.69+
