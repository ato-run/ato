# Identifying a deployed Formation worker

## Why this exists

On 2026-09-11 a staging worker refused sources for a reason that no code on
`main` could produce. Answering "which revision is this?" took: hashing the
binary, comparing source files against `main` (they matched — and that was
misleading, because the tree beside the binary was **not** what it was built
from), grepping the binary for strings, and finally searching git history for a
commit whose error message matched. The binary carried no identity at all.

It does now. The rule that makes it worth trusting: **a build that cannot
determine something reports `unknown`.** It never substitutes a plausible
value, because a binary claiming a commit it was not built from ends an
investigation with the wrong answer — which is exactly what happened here.

## Asking a binary what it is

```console
$ ato-formation-worker --version
ato-formation-worker 0.1.0 commit=264d5c46… clean build_id=ci-run-12345 rustc 1.96.0 (ac68faa20 2026-05-25)
```

The same line is the first thing `serve` writes to its journal, before anything
can fail:

```console
$ sudo journalctl -u ato-formation-worker.service -o cat | head -2
[formation] ato-formation-worker 0.1.0 commit=264d5c46… clean build_id=ci-run-12345 rustc 1.96.0 (…)
[formation] serving api=https://staging.api.ato.run worker_id=formation-sugamo
```

| Field | Meaning |
|---|---|
| `commit` | `git rev-parse HEAD` at build time, or `unknown` |
| dirty flag | `clean`, `dirty`, or `unknown`. Three values, not a bool: "not dirty" and "could not tell" are different claims |
| `build_id` | Whatever the build system calls this build, via `ATO_BUILD_ID`. `unknown` for a developer build |
| `rustc` | The compiler that produced it |

A `dirty` or `unknown` build is fine on a developer machine and is a **release
blocker** anywhere else: neither can be reproduced from a commit alone.

## Pairing a running binary with a deployment record

`--version` says what the binary *claims*. The SHA-256 says which bytes are
actually on the host. Record both, at deploy time:

```console
$ sha256sum /path/to/ato-formation-worker
5ef4a06c…  /path/to/ato-formation-worker
$ /path/to/ato-formation-worker --version
ato-formation-worker 0.1.0 commit=… clean build_id=… rustc …
```

Keep the pair with the deployment. Later, on the host:

```console
$ systemctl --user show ato-formation-worker.service -p ExecMainPID -p FragmentPath
$ sha256sum "$(systemctl --user show ato-formation-worker.service -p ExecStart --value | awk '{print $2}')"
```

If the SHA matches the record, the `--version` line in the record describes the
running binary. If it does not, the host is running something nobody deployed —
which is the situation this document was written after.

**Do not infer identity from the source tree next to the binary.** On
`ubuntu-sugamo` that tree was byte-identical to `main` for three of four
formation files while the binary contained code from an unmerged branch, and
every source file was *older* than the binary, so mtimes did not reveal it
either. The tree tells you what is at that path; only the SHA and the stamped
identity tell you what is running.

## Binaries built before this landed

They have no identity to print. For one of those, in order of reliability:

1. `sha256sum` it and look for a deployment record with that hash.
2. `strings` it for a message that exists only on one branch, then
   `git log --all -S "<that message>"`.
3. `sudo journalctl _PID=<pid>` — the worker's own failure lines are often the
   only surviving description of what it does.

The preserved evidence for the 2026-09-11 investigation
(`sha256:5ef4a06ca3ba77a2c328f63486e2ca42f7c32dcd38a4fa0b8d5525b91fe26f10`,
built 2026-09-04 23:40 UTC, containing `dbb9b9c8`) is filed with that work.
