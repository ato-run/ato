# Owned Terminal browser joint acceptance — 2026-10-24

> Status: historical evidence / past plan. Language: en.
> Preserved content; relative navigation updated. Current [documentation map](../../../README.md).

## Scope and source

This is a test-only joint fixture: real Chrome and the production PWA xterm
component, a test-owned HTTPS/auth proxy, the actual connected Worker lease
entry, isolated CPU Docker/PTY, the existing Native gateway/probe, Rust Contract
verification, and the existing output-save protocol against a fake control
plane. It is not authenticated Hosted/staging acceptance or a deployed Runner.
No production, policy, IPC/Core, API/PWA, K or launch-spec implementation changed
in this Rust PR.

- Frozen base: `integration/native-terminal-browser-fixture-base` =
  `6e0ad7acea83d8656c5873d48cc5d3151adf854f` (placement policy PR #1544).
- Initial test implementation: `b6aee46b6`.
- Corrected frozen implementation:
  `b69b7cc6b6f142e75f8ec5bf46a290a972b6f8b7`.
- Changed Rust: `native_dispatch_tests.rs` and its new
  `native_dispatch_tests/terminal_browser_fixture.rs` child only.
- Independent read-only review: both content hashes matched manifest SHA-256
  `3a9310254f685fc64f8f036427509df65fb0808af2a49ff041298d774e1be75b`;
  no new P1/P2. The reviewer did not execute tests. The four placement-policy
  files still match PR #1544's `40f96c95...` review manifest.

The PWA companion is independently owned. Its frozen test code was
`a6ea247853de275057564319ce4d387b2c9ceb17`; the Root operator executed the actual
browser joint case and provided the browser/orchestration evidence below.

## Fixed contract and actual observations

The original Terminal K remains exact `go\r\n1\r\nEND`. The fixture does not
recapture K or substitute measured bytes. Before exposing Ready, Rust verifies
that K and a scoped authenticated `ato.terminal.v1` Ready control at 80×24.
This readiness connection sends no input or resize. The old test's manual
`go` to advance to 2 is skipped only in this new held browser case.

The test-only Python workload retains the existing `go` counter and `size`
command, additionally atomically writing its counter, `os.get_terminal_size(0)`
and guest PID into the one granted output `status.json`. The initial status is
counter 1, 80×24. Browser xterm sends `go` → 2, resizes its real PTY to 120×40,
reads `size:120x40`, reconnects to the same Surface, sends `go` → 3 and reads
120×40 again. Rust's final verification is a **passive file read**: it opens
no PTY, sends no extra `go`, and cannot repair the expected dimensions. The
workload PID remains 1 in the same owned container. Runtime continuity evidence
is combined with the PWA's two actual authenticated WebSocket connections; the
Rust `runtime_reconnect_verified` fact alone is not a browser socket count.

Ready exposes only fake IDs, the loopback gateway/origin, initial dimensions,
absolute expiry and private handoff file paths. Fixture key and tokens are not
written to handoff/results or injected into the workload. The User assertion
uses existing audience, execution scope and key ID `ato.runner-surface@1`;
Terminal carries no Pixel clipboard/resize permission claims. Origin is
`https://127.0.0.1:4181` and the exact Terminal subprotocol is required.

The absolute 90-second lease/test cap, ordinary 10-second heartbeat, actual
stop/save/recovery and exact scoped authorization are reused. Done is either
exact `{version:1,action:"abort"}` or the five-field fixed request
`{version:1,action:"verify-and-stop",expected_counter:3,expected_cols:120,expected_rows:40}`.
Wrong/unknown/duplicate fields, oversized input, symlinks, non-regular files,
wrong UID or mode other than 0600 are refused. Ready/result publish as 0600
create-new hard links; a previous file cannot be replaced. Unknown observations
stay null; a later failure does not erase an already observed true K/save fact.
Final success requires all expected counter/dimensions, continuity, original K,
frozen GET, output-save and complete cleanup facts together.

## Actual joint result

Root's **first positive browser joint case passed**, with the corrected frozen
binary and no additional run/build after it:

- Chrome `155.0.8059.86`, production xterm component, two browser WebSockets.
- Counter 2 before reconnect, counter 3 afterward, real PTY 120×40, 64 render
  acknowledgement bytes, zero clipboard reads/errors/external origins.
- Rust exact 18-field result: counter 3, 120×40, original K, unchanged frozen
  probe GET, same-workload continuity, output saved and both cleanup facts true.
- Same guest PID 1 before/after; owned container
  `206a262e0822d9748e491ca8ac1eaadfb107c773eeedd0270acd294562183461`.
- Runner `native-dispatch-runner-1001867`, lease
  `native-dispatch-1001867-2e422da77f68a899`, session
  `run:run_01NATIVEWIRE000000000000`, Surface `native-terminal`.
- Selected D, fixed before execution:
  `sha256:5b9d86a3eead50aae0caedadce7241ce38f525016d1a11d576b19542b5065391`.
- Runtime preparation field 0 ms (there is no Pixel-helper preparation on this
  path), lease evaluation 8,437 ms, Rust exact test 8.77 s, Root orchestration
  11.482 s. **These are not startup performance measurements.**
- Stop was confirmed **forced exit 137**, followed by saved `status.json` and
  complete resource cleanup; this is not evidence of graceful shutdown.

Independent read-only postflight found zero running/stopped containers and
networks with that exact Runner owner label, no runtime PID 1001867, closed
remote gateway 33427 and internal probe 41841, closed local HTTPS 4181 and no
matching fixture SSH/Node processes. Root independently observed browser and
runtime exit 0 and removed its SSH control. No installed service or shared
configuration changed.

## Negative evidence and regression checks

The first attempted abort fixture failed **before Ready** due to a test-only
Pixel assumption: the existing wrong-UID preflight test expected workspace to
remain absent. Pixel has a helper preflight, whereas Terminal refuses through
common OCI admission after materialization. Its start-failure output was not
the new Terminal status output, so the fake API assertion failed. That failure
and its failed/null result are retained; it is not counted as successful
Terminal admission or hidden by a production change. The wrong-UID preflight
case remains for Pixel, and existing common OCI wrong-user tests are unchanged.

After restricting only that preflight assertion to Pixel, the corrected
fixture held Ready for 12 seconds before explicit abort. The final same-source
run exited 101 **1,653 ms after abort**, total 14,945 ms. Its strict result was
failed, prior K true and output saved true, unobserved counter/size/reconnect/
frozen facts null, all cleanup true, and all IDs matched Ready. A preparatory
corrected abort case also passed (2,133 ms after abort); both logs remain.

- macOS Worker all-target tests: **192 passed / 20 ignored / 0 failed**.
- Linux same-source Worker regression binary: **193 passed / 23 ignored /
  0 failed**, fresh short own env root and same-source sandbox-shim binary.
- New handoff tests cover fixed/strict done, abort refusal, private atomic
  publication, no overwrite, symlink and public-mode refusal, and origin.
- macOS and Linux Worker all-target Clippy `-D warnings`: passed; fmt/diff passed.
- Actual joint test was run explicitly; it remains ignored by default.

A transfer was initially extracted before completion (tar EOF; no build used
that partial source). It was replaced by a new empty source directory and a
verified archive. A post-Clippy diagnostic used the wrong relative launcher
path; Clippy itself and subsequent input checks had already exited 0. Correct
launcher/hash verification subsequently exited 0. Neither is a code test pass.

## Frozen Linux artifacts and source closure

Own source:
`/home/ekohsuke/ato-run/.tmp/terminal-browser-20261024/source-v2`.
Archive SHA-256:
`66aeedeafba35ffe024ec8e1158738d6f33f30699925dc715e63c60d551e202d`.
The 662-input source/config/literal manifest SHA-256 is
`8a79c72f79b4314a75d1e730996b270ef44a81cbf29b929c5376b25a9d93e82d`.
All content hashes matched before build, after mtime refresh and after actual
acceptance. The earlier count 574 selected only a narrow Rust/Cargo glob;
closure checking resolved 83 literal include paths and included the required
`docs/ops/formation-efficacy-e1-plan.json`. No missing input was supplied from
another worktree/cache. Docs/images were otherwise excluded from the archive.

Immutable own test binary (0555), 57,699,736 bytes:
`bin/terminal-worker-tests-b69b7cc6b`, SHA-256
`322fc33cbb733b7b278d3a474fc014e0825ca55c55a11cc5cd5cec2a0e2fd69c`.

Own prepare-only Worker artifact (0555), 50,490,512 bytes:
`bin/ato-connected-realization-worker-b69b7cc6b`, SHA-256
`0586494e6cb3bac11a00624a6948fdeaf62122e4953746067d5c0c7cc4c7081c`.
It was built from the same v2 source with Cargo dev/unoptimized, debug info 0,
incremental 0, jobs 2, offline/locked. Inert `--help` under a fresh whitelisted
environment exited 0 and contains `--require-explicit-runtime-placement`;
Clap exits before Worker initialization. The artifact was not installed.

Launcher `launch.sh` SHA-256:
`d2bc364741ba245ae3a172bd120fd767db18154347a5829af0110779e1be9991`.
It creates only a new marked `/opt/ato/.tmp/native-terminal-browser.<suffix>`;
Ready/done/result are under its `browser/`. Existing `/opt/ato` (0755) and
`.tmp` (0711) owner/modes remained unchanged. The root-start workload uses the
existing trusted nonroot UID/GID 65534, read-only workspace/inputs, owned output,
no privileged container, host display, Docker socket or credentials. Real keys,
Runner enrollment/installation, flags, Hosted owner Runs, deployment and paid
resources were not used. The shared Linux Cargo target was explicitly released.

Raw evidence is retained in this worktree's `.tmp/terminal-linux-logs/` and
`.tmp/terminal-joint-positive/`. Root's browser evidence is in its PWA
`.tmp/native-terminal-browser/browser-result.json` and joint orchestration
`.tmp/ntbj-f3jkzkwe/`; those are a separate observation authority. This result
adds a reproducible transport/runtime test, not authenticated staging acceptance.
