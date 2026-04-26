# runtime-version-pinning

This sample pins `runtime_version = "3.11.10"` in a `source/python` capsule. Running `ato run .` resolves the exact CPython 3.11.10 interpreter (visible in the provisioning output: "Using CPython 3.11.10") and then reaches E301, meaning the manifest and execution plan are fully valid — only the sandbox opt-in consent is missing. It demonstrates that ato respects exact patch-level Python version pins rather than defaulting to the system interpreter.
