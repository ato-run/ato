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
# 実Coordinatorで見つかったrecipe保存境界

changedetection.ioのwheel-only解決失敗に続くsdist候補は、同じ登録済み
Python helperとlock helperを各phaseへ展開したため、既存64 KiBの
`capsule_toml`保存制約を超え、実行前にCoordinatorが500を返した。
この実測は保全し、成功・source不具合として数えない。

読みやすいhelper原本から固定loaderを生成し、同じPython標準libraryで
原本のbyteを復元して実行する。network、phase、sandbox、toolchain、
deadline、wheel hash検証は同じ経路のまま。Rust/WASMが同一loaderを使う。
生成物は`generate-python-operations.py --check`で原本との一致を検証する。
保存上限を超える候補はRust authorityが`proposal_recipe_byte_limit`として
拒否し、SQLエラーになる前にtyped outcomeへ戻す。

関連33 tests、compact loaderを使った実pip sdist→wheel・hash固定offline
install・cleanup fixture、実npm lifecycle fixtureはPASS。fixtureは実
Coordinator・Runtimeでのchangedetection.io受け入れ検証と区別する。

## Subsequent real-application boundary findings

The changedetection.io native attempt at Ato `3f07cd8e` / API `c7f990ce`
failed before sdist building: setuptools 83.0.0 contains vendored nested
`.dist-info/METADATA`, which the dependency-lock operation mistook for a second
outer-wheel identity. Select only root wheel metadata; retain the digest of the
entire wheel. The real pip fixture now includes vendored metadata and passes.
This operation fixture does not establish changedetection.io Runtime acceptance.

Prompt 12 / capability schema 4 adds `setup_scripts` for the source-owned Node
launch route. Preparation runs inside the ordinary Runtime with its state and
private variables already attached, before launch; the frozen manifest and all
script names are checked before effects. Setup failure prevents launch, and the
existing process-group teardown includes preparation children. Empty preparation
preserves older D bytes and prompt/capability projections 1–11. The fixture
verifies shared state/private environment, refusal after manifest change, and
setup failure without launch. Relevant compiler tests: 33 PASS; Worker reasoning
tests: 54 PASS. Real Kutt native/migration/state acceptance is still pending.
