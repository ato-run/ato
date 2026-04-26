# no-raw-gpu-handle

Attempts to acquire a raw GPU handle by directly `dlopen`-ing the CUDA driver library (`libcuda.so.1`), bypassing ato's GPU broker. This always fails with an `OSError` — either because the library is not installed (most machines) or because a Tier 2+ sandbox denies the privileged open.

## What this proves

Direct GPU access without going through the capsule GPU broker is not possible. Applications must declare `[gpu]` in `capsule.toml` and use ato's broker API; they cannot bypass it via raw FFI.

## Expected output

```
EXPECTED: direct dlopen blocked for libcuda.so.1: ...
EXPECTED: direct dlopen blocked for libcuda.so: ...
EXPECTED: direct dlopen blocked for /usr/local/cuda/lib64/libcuda.so.1: ...
Result: no raw GPU handle could be acquired (expected behavior)
```

## Platform notes

- **macOS**: `libcuda` is not present; `OSError: dlopen(libcuda.so.1, ...)` — library not found
- **Linux (no GPU)**: same — library not found
- **Linux (with NVIDIA GPU, Tier 2 sandbox)**: sandbox denies the open with `PermissionError`
- **Linux (with NVIDIA GPU, no sandbox / `--dangerously-skip-permissions`)**: `OSError` may or may not occur depending on driver installation

In all production paths, raw GPU access is blocked.
