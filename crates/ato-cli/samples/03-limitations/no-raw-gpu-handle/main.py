"""
Attempts to acquire a raw GPU handle by directly dlopen-ing the CUDA driver library,
bypassing ato's GPU broker. This always fails — either because libcuda is not present
(most machines) or because the sandbox denies the privileged open.
"""

import ctypes
import sys

CUDA_LIBS = ["libcuda.so.1", "libcuda.so", "/usr/local/cuda/lib64/libcuda.so.1"]

for lib in CUDA_LIBS:
    try:
        handle = ctypes.CDLL(lib)
        # If we somehow get here, the raw handle was acquired — unexpected.
        print(f"UNEXPECTED: raw GPU handle acquired via {lib}", file=sys.stderr)
        sys.exit(1)
    except OSError as e:
        print(f"EXPECTED: direct dlopen blocked for {lib}: {e}")

print("Result: no raw GPU handle could be acquired (expected behavior)")
sys.exit(0)
