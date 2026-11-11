# DataResource local E2E (ato runtime / CLI / Coordinator), 2026-11-11

Status: **local verification only**. No staging or production resource was read or
mutated. Every result below was produced on this macOS arm64 host by a command in
this document; steps that could not run are marked BLOCKED, never PASS.

## 1. What was verified

| Area | Result |
|---|---|
| `origin/stg` carries the DataResource runtime work (#1734, #1749, #1760, #1763, #1775) | **PASS** (see 2) |
| Coordinator conformance suite (`cases.json` / `tests/conformance.rs`) | **PASS 15/15 cases**, 1+3 crate tests |
| E2E-D: SQLite Coordinator + TODO/Calendar/Excalidraw workspace, save, checkpoint, restart, compare, Coordinator process restart | **PASS 30/30 steps** |
| E2E-E: one Resource read by two clients, CLI update observed, persistence across a Coordinator restart | **PASS 12/12**, Desktop half **BLOCKED** |
| E2E-C: sample-level selective team sharing (todo_list_shared / todo_set_shared) on the real SQLite Coordinator | **PASS** (repo harness + 8 extra assertions) |
| Existing harnesses: `scripts/oss-quick-start/acceptance.sh`, `services/coordinator/e2e/saved-state-roundtrip.mjs` | **PASS** |

## 2. Reachability audit of the DataResource PRs on `origin/stg`

Checked at 2026-11-11 22:0x JST after `git fetch origin --prune`:

| ref | commit |
|---|---|
| `origin/stg` | `4b25185d1fc9043d5a28583dc7ccc6354a536711` (merge PR #1793 from dev) |
| `origin/dev` | `345f71e3c4657ab64dc5cebc902b4c8c98da114b` |
| `origin/main` | `271b603c61d22d02ec9acca1344bba06279c2fcf` |
| `origin/nightly` | `c5e1560f04a8c2852505f4c182a53363d217ad30` |
| local checkout (`apps/ato`) | nightly `65cec62b`, 262 commits behind `origin/dev`, has no `services/coordinator` |

### 2.1 #1734 and #1749 are present, under rewritten commit SHAs

`gh pr view` reports merge commits `c2eeb5262` (#1734) and `d54abd7f` (#1749).
Neither is an ancestor of `origin/stg`, `origin/dev`, `origin/main` or
`origin/nightly`, and `7f2a1b658` (the #1734 content commit) is carried only by
local stale branches. **The content is nevertheless fully present:**

- #1734 — equivalent commits `e0453088f` (content) and `ba843ea7b` (merge) exist
  and are ancestors of `origin/stg`, `origin/dev` and `origin/main`.
  `git diff 7f2a1b658 e0453088f --stat` is empty (identical trees); the raw commit
  objects differ only in the parent line (`5f1647422` vs `c6d764ff4`, which are
  themselves tree-identical).
- #1749 — equivalent merge `b5c29389f` ("Merge pull request #1749 from
  ato-run/dev", same author timestamp) is an ancestor of `origin/stg` and
  `origin/main` (not `origin/dev`, as expected for a dev→stg promotion merge).
  Both merges have tree `1e7495cce922a88ff69f3b60ff749965a26c1efb` and their two
  parent trees are equal.
- Cause: the repository history was rewritten after those merges. Pre-rewrite
  merge objects carry `author Kohsuke <149476241+Koh0920@users.noreply.github.com>`
  / `committer GitHub <noreply@github.com>`; the post-rewrite objects carry
  `koh0920 <kohsuke20010920@gmail.com>`. Only commit objects changed (children
  re-parented); trees are byte-identical. GitHub's PR metadata still points at the
  pre-rewrite SHAs, which is why they look unreachable. **No DataResource content is
  missing from `origin/stg`.**
- Content spot checks in the `origin/stg` tree: `apps/cli/src/data.rs` carries
  `--origin` / `ATO_COORDINATOR_ORIGIN` and the
  `data_requests_carry_the_first_party_origin_when_asked` test;
  `apps/cli/src/workspace.rs`, `apps/cli/tests/workspace.rs`,
  `docs/guides/quick-start-first-workspace.md`, `services/coordinator/**` all exist.

### 2.2 #1760, #1763, #1775 and the releases

| PR | merge | in stg | in dev | in main |
|---|---|---|---|---|
| #1760 3-app workspace (`476191f3f`) | `476191f3f` | yes | yes | yes |
| #1763 release promote (`bddfc577b`) | `bddfc577b` | yes | no (promotion commit) | yes |
| #1775 release promote (`d4b12910b`) | `d4b12910b` | yes | no (promotion commit) | yes |

- `origin/stg:apps/cli/Cargo.toml` is `version = "0.9.1"`; release commits
  `6544bcdc5` (0.9.0) and `931497cc0` (0.9.1) are reachable from `origin/stg`.
- Tags `v0.9.0` = `c82eff315`, `v0.9.1` = `787e5be29` are the **stg→main release
  PR merge commits**; they are ancestors of `origin/main` and are not ancestors of
  `origin/stg` by construction (not a gap).
- `origin/nightly` does not contain the Coordinator work; the local `apps/ato`
  checkout (nightly `65cec62b`) has no `services/coordinator`. All runs below used
  a detached worktree of `origin/stg` at
  `.tmp/datare-audit/ato/wt-stg` (workspace `.tmp`, not the nightly checkout).

## 3. Contract and conformance

Read: workspace `docs/rfcs/draft/data-resource-continuation-contract.md` (268 lines,
invariants I1–I12, ProtocolSemantics interface, operation envelope, wire v4 mapping)
and `apps/ato/.worktrees/datare-contract/docs/rfcs/draft/DATARESOURCE_FEDERATION_CONTRACT.md`
(draft, "normative for the cross-surface implementation"; one authority per
Resource, CAS + operation_id commit invariants, operations-only Grant cannot reach
document read, the conformance matrix in §8 names exactly the SQLite/D1 shared
cases). The conformance matrix is the contract the suite below implements.

Toolchain: `cargo 1.96.0` / `rustc 1.96.0`.

```
cd .tmp/datare-audit/ato/wt-stg
cargo test -p ato-coordinator --test conformance -- --nocapture
```

- `tests/conformance.rs::the_sqlite_coordinator_passes_the_shared_conformance_cases`:
  **PASS** (built in 24.6 s). The suite is one Rust test that iterates every case in
  `conformance/cases.json` in order; any failing case panics with its `case id`, and
  the match has no branch that skips a case.
- Per-case evidence: an unmodified copy of the crate under
  `.tmp/datare-audit/ato/conformance-probe` with exactly one added
  `println!("CONFORMANCE_CASE {id} action={action}")` line (assertions untouched;
  `diff` against the repo harness shows only that line) executed all cases:

| # | case id | action | outcome |
|---|---|---|---|
| 1 | `create` | create | PASS |
| 2 | `commit` | commit | PASS |
| 3 | `replay` | commit | PASS |
| 4 | `operation_id_conflict` | commit | PASS |
| 5 | `stale_revision` | commit | PASS |
| 6 | `register_due_dates` | register_operation | PASS |
| 7 | `register_create` | register_operation | PASS |
| 8 | `publisher_conflict` | register_operation | PASS |
| 9 | `bind_operations` | bind | PASS |
| 10 | `document_read_refused` | read_binding_document | PASS |
| 11 | `ungranted_operation` | invoke | PASS |
| 12 | `granted_operation` | invoke | PASS |
| 13 | `revoke` | revoke | PASS |
| 14 | `revoked_invoke` | invoke | PASS |
| 15 | `revoked_document` | read_binding_document | PASS |

- Whole crate: `cargo test -p ato-coordinator` → conformance 1/1, `tests/relay_cli.rs`
  3/3, doc-tests 0. Logs: `.tmp/datare-audit/ato/logs/conformance-test.log`,
  `.tmp/datare-audit/ato/logs/conformance-probe.log`.

Binaries (both built from `origin/stg` `4b25185d1`, debug profile):

| binary | sha256 | version |
|---|---|---|
| `cli-target/debug/ato` | `cdacfc2620e49ef0fe792bcdbe2aa5edf4791cb716772fee2b2d28a5eb98575d` | `ato 0.9.1` |
| `wt-stg/target/debug/ato-coordinator` | `4438472182032dcedea7cf6dc6a8d9ee60d0093c0c5422dcca01a429f747f96b` | `ato-coordinator 0.1.0` |

## 4. E2E-D: SQLite Coordinator + 3-app workspace, checkpoint, restart

Workspace: `todo`, `calendar`, `excalidraw` capsules from the published v0.9.1
quick-start bundle (`capsule sha256: todo cc5627453ee2…, calendar a47f8c9c813c…,
excalidraw 468548c23bcc…`), `ato.workspace/1`, horizontal split `[0.4, 0.3, 0.3]`.

Harness 1 — the existing `scripts/oss-quick-start/acceptance.sh` (fresh open,
bridges, `todo_create`, Calendar projection, checkpoint, resume, split check):

```
bash .tmp/datare-audit/ato/wt-stg/scripts/oss-quick-start/acceptance.sh \
  --ato .tmp/datare-audit/ato/cli-target/debug/ato \
  --capsules <v0.9.1 bundle dir> --port 18846 \
  --workdir .tmp/datare-audit/ato/e2e/d1
```

```
PASS  open: 3391ms  resume: 3673ms  total: 7064ms
```

Harness 2 — `.tmp/datare-audit/ato/e2e/e2e_d.py` (extends the same flow with an
Excalidraw scene save, checkpoint comparison and a Coordinator process restart):

| step | result |
|---|---|
| D1 fresh open (shell HTTP 200) | PASS |
| D2–D3 3 pane iframes, split `[0.4,0.3,0.3]` embedded | PASS |
| D4 per-pane bridges `todo/tasks`, `calendar/tasks`, `excalidraw/scene` | PASS |
| D5 Calendar is `access=operations`, `["todo_list_due_dates"]` | PASS |
| D6 `todo_create` → outcome revision 2 | PASS |
| D7 Calendar projection contains the task (`task-op-1`, due `2026-12-25`) | PASS |
| D8 scene starts empty (0 elements) | PASS |
| D9 `scene_save` (2 elements) → outcome revision 2, `element_count 2` | PASS |
| D10 `scene_load` returns both elements | PASS |
| D11 Stop and save wrote `ck-fresh.toml` (4,278 bytes) | PASS |
| D12 checkpoint layout `[0.4, 0.3, 0.3]`, 3 panes | PASS |
| D13 checkpoint content: task title present, 2 scene elements; todo rev 2 / `sha256:eaaa181153ee65c76441944552436fc7748e19c2bb9cee216f627c333f18dcf0`, scene rev 2 / `sha256:895e9d88f495e02c9dc3d9fdb128542bbb403ce4c318d539c8a3185ed9f0af4a` | PASS |
| D14 resume open from the checkpoint | PASS |
| D15 3 iframes and the saved split after resume | PASS |
| D16 Calendar projection still has the task | PASS |
| D17 Excalidraw scene still has both elements | PASS |
| D18 resume Stop wrote `ck-resumed.toml` | PASS |
| D19 layout identical (`[0.4, 0.3, 0.3]`) | PASS |
| D20 content digests identical to D13 | PASS |
| D21 revisions observed: fresh `todo=2 scene=2` → resumed `todo=1 scene=1` (see §7.1) | PASS (observed) |
| D22 per-pane bindings + ResourceIds on both runs | PASS |
| D23–D25 external SQLite Coordinator: health, create (rev 1, `sha256:700746b0…`), read | PASS |
| D26–D27 kill the Coordinator process and start a new one on the same DB: same revision 1, same `content_digest`, document title intact | PASS |
| D28 list after restart returns the one Resource | PASS |

`RESULT D PASS steps=30 failed=[]`.
Logs: `.tmp/datare-audit/ato/logs/e2e-d-extended.log`, `e2e-d-acceptance.log`,
`e2e-d-fresh.log`, `e2e-d-resume.log`, `e2e-d-coordinator-{1,2}.log`.
Artifacts: `.tmp/datare-audit/ato/e2e/d2/{ck-fresh.toml,ck-resumed.toml,coordinator-persist.sqlite}`.

## 5. E2E-E: one Resource, two clients, CLI update, persistence

Harness `.tmp/datare-audit/ato/e2e/e2e_e.py`. Client A = the local Coordinator HTTP
API (the surface the Desktop's local coordinator client uses). Client B = the CLI
(`ato data resource inspect`, `ato data operation invoke`). SQLite DB
`e2e-e.sqlite`, port 18891, loopback only.

| step | result |
|---|---|
| E1 Coordinator health | PASS |
| E2 client A creates the Resource (rev 1) | PASS |
| E3 client A reads rev 1 / `sha256:21442106d3695d9b101a238a22bc8fca752e672814f1cbdcce5342284dc3dbde` | PASS |
| E4 CLI reads the same revision and digest | PASS |
| E5 CLI commits an update (rev 2) | PASS |
| E6 client A observes rev 2 / `sha256:237f321fd681c9a421693d46a59222eaae1030ae4f93f665ce4ec05912c976a4`, title "updated from the CLI" | PASS |
| E7 stale CLI write (expected_revision 1) refused `revision_conflict` (HTTP 409) | PASS |
| E8 CLI reads rev 2 + new digest back | PASS |
| E9 Coordinator process restarted on the same DB | PASS |
| E10 client A still sees rev 2 + the same digest | PASS |
| E11 CLI still sees rev 2 + the same digest, document title intact | PASS |
| E12 exactly one Resource after restart | PASS |
| E13 Desktop-specific half | **BLOCKED** |

`RESULT E PASS steps=13 failed=[] blocked=['E13-desktop-half']`.
Log: `.tmp/datare-audit/ato/logs/e2e-e.log`.

BLOCKED basis for the Desktop half: `ato-desktop` (branch
`feat/coop-end-to-end-trace`, `9e5eda6`) is a Tauri WebView application (no
headless mode); `e2e/local/README.md` says the lane that drives the real Tauri
application is **"Status: empty. Nothing here yet."**; and its own notes state
`tauri-driver` v2.x exits with "tauri-driver is not supported on this platform" on
macOS. Nothing was executed for that half, so it is not claimed.

Additional two-client evidence from an existing harness,
`services/coordinator/e2e/saved-state-roundtrip.mjs` (real SQLite Coordinator,
sample handler from `ato-samples`): export-set → import-set keeps the document
digest, the imported state is live for the CLI and MCP on the same revision 2, and
a tampered archive is refused before creating anything — **PASS** (log
`.tmp/datare-audit/ato/logs/e2e-saved-state.log`).

## 5b. E2E-C: sample-level selective team sharing on the real SQLite Coordinator

The Lead asked for a sample-level proof to sit next to the hosted-API one. `ato-samples` merged #10 into `dev` and promoted it to `ato-samples` `stg`
(`fc014b9ade2ac7e30b2c4b20a234f5f5017c8675`, promotion merge #48), adding the
operations `todo_list_shared` (observation, read) and `todo_set_shared`
(evolution, read_write) to `samples/shared-json-todo/src/operations/todo.js`.

ato-samples was **not modified**: the handler was read out of `origin/stg` with
`git show` into the workspace scratch, and the harness is the repository's own
`services/coordinator/e2e/team-sharing.mjs` (byte-identical to the workspace copy
`.tmp/team-sharing-e2e.mjs`).

```
cd .tmp/datare-audit/ato
COORD=$PWD/wt-stg/target/debug/ato-coordinator
$COORD init  --db e2e/c/team-share.sqlite
$COORD serve --db e2e/c/team-share.sqlite --addr 127.0.0.1:8799 --token smoke &
git -C ../../apps/ato-samples show origin/stg:samples/shared-json-todo/src/operations/todo.js \
  > samples-stg/todo.js        # sha256 4209d1585de25daa13443243dfd4678a0aaefdfe3883c061ba785a24a414cd15
SAMPLE_HANDLER=$PWD/samples-stg/todo.js \
  node wt-stg/services/coordinator/e2e/team-sharing.mjs
```

Repository harness — **PASS** (exit 0):

```
consumer sees: [{"due_at":"2026-12-01","number":1,"status":"todo","task_id":"a","title":"Team visible"}]
consumer invoke unlisted operation: 403 {"error":"operation_not_granted"}
owner unshared a: revision 2
consumer sees after unshare: []
board keeps both tasks, revision 2 digest sha256:9bf2308cadb75
PASS: selective sharing is operation-scoped and revocable; revision 1 task visible
```

The harness checks the happy path but not the exact field set or an unknown id, so
the same harness was extended (assertions only, same real Coordinator + real sample
handler, fresh database) by `.tmp/datare-audit/ato/e2e/e2e_c_extra.mjs`:

```
$COORD init  --db e2e/c/team-share-extra.sqlite
$COORD serve --db e2e/c/team-share-extra.sqlite --addr 127.0.0.1:8799 --token smoke &
SAMPLE_HANDLER=$PWD/samples-stg/todo.js node e2e/e2e_c_extra.mjs
```

| step | result | observation |
|---|---|---|
| C1 consumer Port sees the shared task | PASS | `task_id a` |
| C2 exactly the five declared fields | PASS | `["due_at","number","status","task_id","title"]` = `{"due_at":"2026-12-01","number":1,"status":"todo","task_id":"a","title":"Team visible"}` |
| C3 private task hidden | PASS | consumer task ids `["a"]` only |
| C4 description / subtasks not leaked | PASS | projection has neither; the board document still stores `description` for both tasks |
| C5 consumer cannot evolve | PASS | `403 {"error":"operation_not_granted"}` |
| C6 unshare revokes on the next call while the board advances | PASS | revision 1 → 2, consumer `[]`, board keeps task `a` with `shared=false`, digest `sha256:f0e6fd77be800b0af847…` |
| C7 board revision keeps advancing | PASS | revision 2 → 3 (task `b` shared), consumer sees only `b`, digest `sha256:644dda3c035a5cf9b29e…` |
| C8 unknown id fails closed | PASS | `500 {"error":"handler_failed"}`, revision and `content_digest` unchanged |

`RESULT C PASS steps=8 failed=[]`. Logs:
`.tmp/datare-audit/ato/logs/e2e-c-team-sharing.log`,
`.tmp/datare-audit/ato/logs/e2e-c-extra.log`,
`.tmp/datare-audit/ato/logs/e2e-c-coordinator{,-extra}.log`.

Detected, not fixed (§7.3): a handler raising "TODO not found" for a
caller-supplied unknown id surfaces as **HTTP 500 `handler_failed`** rather than a
client-visible 4xx. It is fail-closed (no revision or digest change).

## 6. Not covered

- No staging or production Coordinator/API was contacted; no hosted D1
  conformance run is recorded here (ato-api owns that half).
- No real browser session: pane calls were made through the same
  `/apps/<pane>/__ato/data-ports/<port>/invoke` bridge the shell uses.
- No Desktop run (§5, BLOCKED). No cross-host continuation, Writer-lease
  hand-over or two live Coordinators.
- Only `--local-data` and standalone `ato-coordinator serve` were exercised;
  `ato workspace open --coordinator <url>` was probed only for §7.2.

## 7. Detected mismatches (detected, not fixed)

### 7.1 Detected, not fixed: a resumed workspace mints new ResourceIds and restarts revisions (content survives)

The checkpoint stores per-Resource `revision` and `content_digest` (`[data.resources]`).
On resume the CLI creates the documents again on a fresh in-process Coordinator, so
the same document comes back with the **same `content_digest`** (D20) but a new
`resource_id` and `revision` 1 instead of 2 (D21/D22). This matches the federation
contract's rule that copying data with a new ID is an explicit export/import, not
authority transfer, so it is not wrong by that rule. What it does not do is the
authority-move form of the same contract (§5: "Moving the authority keeps the
ResourceId and fences the old authority with an epoch"): no ResourceId is carried
across the checkpoint and no old-authority epoch is recorded anywhere in the
checkpoint. Two consequences to settle before anyone builds "resume on another
machine" on top of it: (a) "revision equality across a checkpoint" is not a valid
expectation and must not be asserted by future harnesses, and (b) a consumer that
addresses a Resource by `resource_id` loses the handle after a resume.

### 7.2 Detected, not fixed: a workspace pointed at an external Coordinator has no Data Ports bridge

`docs/guides/quick-start-first-workspace.md` ("Continue on another computer") says two
machines can point at one Coordinator with
`ato workspace open workspace.toml --coordinator http://<coordinator>:8788 --token <token>`.
Probed on 2026-11-11: the Resources are provisioned on that Coordinator
(`todo.board@1: 13 registered`, `excalidraw.scene@1: 3 registered`), but the pane
iframes are served by the CLI's own static servers and
`GET /__ato/data-ports` and `GET /__ato/runtime-context.js` on those pane origins
both return **404** (only the Coordinator's `/apps/<prefix>/` routes serve the
bridge, and that path is only wired when `--local-data` is used). In this build the
documented live-continuation path therefore cannot deliver pane data; the
checkpoint `data.resources` restore loop is also inside the `--local-data` branch
only. Evidence: `.tmp/datare-audit/ato/logs/e2e-external-probe.log`,
`e2e-external-workspace.log`. Detected, not fixed.

### 7.3 Detected, not fixed: an unknown task id is classified as HTTP 500 `handler_failed`

The team-sharing harness (§5b) invokes `todo_set_shared` through the owner Port with
`{"id": "does-not-exist"}`. The SQLite Coordinator answers **500
`{"error":"handler_failed"}`**, and the revision and `content_digest` are unchanged
(fail-closed, so no partial commit). The classification is too coarse: a caller
cannot tell a caller-supplied unknown id from an internal failure, and the same
surface answers `403 operation_not_granted` for an ungranted operation and `409
revision_conflict` for a stale write, so 500 is the odd one out. Evidence:
`.tmp/datare-audit/ato/logs/e2e-c-extra.log`. Detected, not fixed.

## 8. Evidence index (workspace-relative)

```
.tmp/datare-audit/ato/logs/conformance-test.log
.tmp/datare-audit/ato/logs/conformance-probe.log
.tmp/datare-audit/ato/logs/e2e-d-acceptance.log
.tmp/datare-audit/ato/logs/e2e-d-extended.log
.tmp/datare-audit/ato/logs/e2e-d-fresh.log
.tmp/datare-audit/ato/logs/e2e-d-resume.log
.tmp/datare-audit/ato/logs/e2e-d-coordinator-1.log
.tmp/datare-audit/ato/logs/e2e-d-coordinator-2.log
.tmp/datare-audit/ato/logs/e2e-e.log
.tmp/datare-audit/ato/logs/e2e-e-coordinator-1.log
.tmp/datare-audit/ato/logs/e2e-e-coordinator-2.log
.tmp/datare-audit/ato/logs/e2e-saved-state.log
.tmp/datare-audit/ato/logs/e2e-external-probe.log
.tmp/datare-audit/ato/logs/e2e-c-team-sharing.log
.tmp/datare-audit/ato/logs/e2e-c-extra.log
.tmp/datare-audit/ato/logs/e2e-c-coordinator.log
.tmp/datare-audit/ato/logs/e2e-c-coordinator-extra.log
.tmp/datare-audit/ato/e2e/{e2e_d.py,e2e_e.py,e2e_c_extra.mjs,probe_external.py}
.tmp/datare-audit/ato/e2e/d1   (acceptance.sh workdir)
.tmp/datare-audit/ato/e2e/d2   (E2E-D workspace, checkpoints, SQLite DB)
.tmp/datare-audit/ato/e2e/e2   (E2E-E SQLite DB)
.tmp/datare-audit/ato/e2e/c    (E2E-C SQLite DBs)
.tmp/datare-audit/ato/samples-stg/todo.js  (extracted read-only from ato-samples origin/stg fc014b9)
```
