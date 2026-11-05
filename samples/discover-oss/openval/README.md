# OpenVal candidate

This is an **unpublished** candidate for [OpenVal](https://github.com/aanfinson00/openval)
at commit `89f471751e498813c8458cdc1f6bdc5a54b78e2a` (MIT, pre-alpha). The
upstream Next.js frontend is built with `output: "export"`; `server.py`
serves that export and dispatches the three upstream Python handlers for
cashflow calculation and workbook import/export. It does not replace the
calculation engine. Requests larger than 10 MiB are rejected.

`requirements.lock` pins the tested Python dependencies and their hashes.
`bootstrap.py` installs from pre-populated platform-specific wheelhouses
without network access into the Runner's temporary directory. The template
declares the local test's exact Python version, 3.12.8; this is an explicit
runtime requirement and must be checked against the selected hosted Runner.
The candidate requires Linux x86-64 or macOS arm64 wheels, and the selected
Python must support `venv` and `ensurepip`.

To assemble a candidate, place the exact upstream tree, including its `LICENSE`,
in a clean directory; build `web/out` after setting the Next.js export option;
copy `server.py`, `bootstrap.py`, and `requirements.lock` to the tree root,
and copy `capsule.template.toml` to `capsule.toml`. Populate
`wheels/linux-amd64` and `wheels/macos-arm64` with Python 3.12 wheels matching
the lock. Then run `ato pack` against the assembled directory. Do not commit
the upstream tree, wheelhouse or generated bundle into this repository.

For adapter testing, set `OPENVAL_ROOT` to the upstream tree containing the
built `web/out`, install the upstream Python requirements and pytest in a
Python 3.12 environment, then run:

```sh
python -m pytest -q samples/discover-oss/openval/test_server.py
```

On 2026-11-05 the upstream web build succeeded and upstream tests reported
184 passed, 18 skipped. Adapter integration tests passed: sample workbook
parse, cashflow, export and reimport, plus invalid and oversized requests.
The assembled bundle also ran the same functional sequence locally after an
offline, hash-checked install. The assembled tree also passed that sequence in
a Linux x86-64 container with a read-only filesystem, no network, a non-root
user and a temporary writable directory; the Python base image was
`python@sha256:2199a62885a12290dc9c5be3ca0681d367576ab7bf037da120e564723292a2f0`.
With the startup-admission correction in this branch, local `ato run` also
produced a `cli-local` receipt with `fully_satisfied = true` for the page and
all three HTTP API health observations. The workbook round-trip above was
tested separately, not by that receipt. On 2026-11-05, the staging import UI
showed an upload in progress, then returned `portable_runner_unavailable`; no
hosted Run or receipt has been observed. This error alone does not identify
whether the Runner is absent, incompatible, or temporarily unavailable. The
npm audit of the upstream lock reported vulnerabilities, including a critical one;
their applicability to the static export still needs review. Do not publish
as usable until the Runner, admission, security findings and real browser
workflow have been verified. This pre-alpha software must not be represented
as verified financial advice or production-ready underwriting.

Without the startup-admission correction in this branch, the declared startup
gate makes the D's execution requirements non-empty and normal execution is
refused. The gate gives the offline dependency installation up to 120 seconds
to become ready and must not be removed merely to obtain a passing result.
Hosted execution has not been verified for this bundle.

The locally assembled 67 MiB candidate has bundle SHA-256
`142eeae345e790bbb838c253cbabdb21b6d645bff809c73384110e84aa85a6e9`,
ContractRef `7cba0ea3f468ec014fd34cb096fbe3f6851ebf4ceb3ef5d277b95d6e389ef88e`,
and DerivationRef `58ec029dcef2d304374061cacdea888f0639190f9ca13ec497d14c7ee54a4bd4`.
These identify that particular candidate, not later rebuilds. Its tar tree
includes the upstream license and complete pinned source. At this size the
hosted bundle upload requires the direct-upload path to be configured. The
staging import did not complete; the UI did not confirm the stored artifact.
