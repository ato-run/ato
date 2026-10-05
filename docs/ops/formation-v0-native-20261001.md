# Formation v0 native dependency operations — 2026-10-01

Depends on Ato #1454. Fixed merged main is still
`8fca0f4b78b1bed92eae21823e5c5f7207058271`; provider prompt versions 1–9
retain their bytes and legacy capability projections. Version 10 exposes the
new operations, measured tool bindings and explicit phase/network conditions.
Saved pending input uses its original public Runtime snapshot and source proofs;
restart does not replace that request or restore retries, rounds or budget.

`python_build_requirements` consumes a source-owned requirements reference,
exact build-backend dependency versions, selected frozen toolchains and build
network policy. Build backends and their transitive dependencies first become
hash-locked wheel artifacts. A dedicated venv inside the existing Runtime
sandbox installs them offline; source requirements resolve wheel/sdist inputs.
Metadata hooks share the declared dependency acquisition network; wheel builds
use the separately declared denied/scoped-build phase. The finished wheels have
version/hash, acquisition input identities, build dependency identities,
toolchain executable hashes and build provenance. The final application install
uses only these artifacts with `--no-index --require-hashes`. Transient build-env
interpreter symlinks/cache are removed; acquired artifacts, wheels and provenance
remain. No application command is embedded in a preset.

`npm_ci` remains acquisition with scripts disabled. A fresh generated completion
directory and an audit prevent source/stale receipts or ignored required scripts
from being treated as a runnable dependency set. `npm_rebuild` must follow the
same frozen manifest/lock acquisition, explicitly name source/lock-owned packages
and root lifecycle operations, and select toolchains/build network. node-gyp
uses bound Python/gcc/make and prebound Node headers. Scripts run only through
the existing isolated, time/resource/network-limited exec path. Ambient compiler
environment is replaced by explicit bindings; absent tools/headers stop the
operation. Acquisition integrity, metadata hashes and completion evidence remain
with the generated artifact. Source rewriting and undeclared npm workspaces are
not added. A `launch_script` selects an existing source-owned npm startup
script from the frozen package.json reference. The source hash is checked
before launch, and the normal Runtime uses that script without an invented
reference to generated output or an application-specific preset command.

Offline operation fixtures passed real pip/PEP 517 sdist-to-wheel creation,
hash-fixed offline install/import and provenance, and real npm source lifecycle
rebuild, pre-rebuild refusal and stale completion refusal. These fixtures do
not constitute actual Coordinator/Runtime acceptance. Compiler/Worker regression
tests and macOS/Linux clippy results are retained in this task worktree.

changedetection.io/Kutt startup, representative functionality, migration/state,
fault cases and Codex/API acceptance remain required after the input path and
final execution pin are complete. New live provider calls: zero. No deployment,
remote migration, flag change, normal Run permission or publication occurred.
Old measurements and WBO UNKNOWN are retained.

Operation semantics follow the primary [pip wheel documentation](https://pip.pypa.io/en/stable/cli/pip_wheel/)
and [npm rebuild documentation](https://docs.npmjs.com/cli/v10/commands/npm-rebuild/).
