# Desktop launch-condition core — device-local backend completion audit

_Status as of 2026-06-06. Scope: the **device-local backend/core** for Capsule
launch conditions. The Desktop **GUI experience is NOT yet complete** — the
GPUI/rfd folder-picker that consumes the #560 backend seam is a required
follow-up (see Deferred). Cross-device (#509), cloud secret store, GC (#550), and
strict repair (#551) are out of scope._

Core principle throughout: **prompt is not proof** — a ref/claim is written only
after the real value/target exists; raw secret values and raw host paths never
enter the ledger, receipts, logs, or errors; relaunch resolution never rereads
the manifest/lockfile.

## Target experience (device-local backend/core: met — GUI picker still required)

```
User opens / relaunches an installed Capsule
  -> Ato reads the installed-state DB as the local SOT
  -> resolves missing launch conditions locally
  -> user satisfies secret / state / port / env conditions via Desktop/CLI inputs
  -> Ato records proof only after real local values/targets exist
  -> relaunches without rereading scattered lockfiles/manifests
```

The **CLI** path achieves this today. On **Desktop**, the backend seam (#560)
surfaces an unresolved `state.*` (`state_binding_required`) and the
`resolve_state_binding_from_path` core API satisfies it — but the **GUI
folder-picker that drives it is a required follow-up**; until it ships, the
Desktop GUI experience is not yet end-to-end complete.

## Status by condition kind

| Capability | Status | PRs |
|---|---|---|
| `secret.*=prompt` create + inject (web/source) | done | #544/#545/#546 |
| `secret.*=grant` inject into OCI containers | done | #553 |
| Manifest-free `state_binding_targets` value store | done | #552 |
| `state.*=prompt` real creation (target→ref→`binding:<id>`) | done | #555 (closes #547) |
| **`state.*=binding:<id>` runtime materialization (mount / `ATO_STATE_<KEY>`)** | done | **#559** |
| **`capsule://?port=` / `port.main=` → launch-time PortClaim** | done | **#556** (#548) |
| **`env.K=grant:<id>` inject via receipt-excluded `secret_env`** | done | **#558** (#549) |
| **Desktop backend seam**: `resolve_state_binding_from_path` + preflight `state_binding_required` | done | **#560** (#404 backend) |
| Desktop **GPUI folder-picker UI** for unresolved `state.*` | **follow-up** | (consumes #560 seam; add `rfd`, render in unified resolution modal, verify via ato-desktop MCP/AODD) |
| `env.K=prompt` (sensitive env via interactive prompt) | follow-up | #557 |

### How the pieces fit
- **State**: at install, the state extractor records the non-sensitive guest
  `mount_target` in the claim `detail_json` (#559) so relaunch needs no manifest.
  `state.*=prompt` (#555) / the Desktop seam (#560) record a local-private target
  (#552) then the `state_binding_ref` proof, and rewrite to `binding:<id>`. At
  relaunch, `resolve_state_binding_materialization` (#559) reads the target with
  an install-profile ownership check and attaches a **receipt-excluded
  `state_mounts`** channel (distinct from `injected_mounts`, which the receipt
  observes); executors apply it only at the spawn boundary (source/web via
  `ATO_STATE_<KEY>`, OCI via a bind mount). The raw host path never leaves the
  local-private target store.
- **Port**: `port`/`port.main` query inputs become the preferred port for the
  installed web-service launch; `auto` makes no concrete claim; conflicts use the
  existing remap policy; `ato run` is unaffected (#556).
- **Env**: `env.K=grant:<id>` reuses the SecretStore-backed, receipt-excluded
  `secret_env` channel and the install-profile ownership/value-present checks
  (#558).
- **Desktop**: `ato internal preflight --json` now emits a typed
  `state_binding_required { state_key, label }` (never a path) for unresolved
  `state.*`, and `resolve_state_binding_from_path` (capsule-core, callable from
  ato-desktop which doesn't link ato-cli) records target→ref and returns the
  `binding:<id>` to re-submit — the exact seam the GPUI picker will call (#560).

## Deferred (clearly NOT basic device-local Desktop UX)
- Desktop GPUI/rfd folder-picker UI (follow-up; consumes #560).
- `env.K=prompt` interactive sensitive-env prompt — #557.
- Cross-device placement / provider snapshots — #509.
- Materialized object size reconciliation / ref-count / GC — #550.
- Strict relaunch / repair / remap / re-placement — #551.
- Cloud secret/state store.

## Verification

Per-PR: each unit's `cargo test -p <crate> --lib <filter>` suites pass and
`cargo check --workspace --all-targets` is green on its branch (see each PR).

Integration + hermetic CLI smoke (the four PRs assembled onto `dev` in merge
order #559 → #556 → #558 → #560, isolated `ATO_HOME`):

**Integration: PASS.** The four PRs merge cleanly — only #556 conflicted, exactly
as predicted and purely additive (`launch_context.rs`: `state_mounts` vs
`port_preferences` field/initializer/accessor; `run.rs`: the state-binding gate
vs the port-preferences collect step), resolved by union (independent channels,
no shared state). No conflict markers remain; no semantic conflict; no bug found.

**Build + integrated tests: PASS.** `cargo build -p ato-cli` and
`cargo check --workspace --all-targets` (incl. ato-desktop targets touched by
#560) are clean on the integrated tree. Focused suites on the integrated tree:
`secret_injection` 19, `state_binding` 25, `port` 216, `preflight` 78,
`capsule-core installed_state` 177, `relaunch` 27, `installed_relaunch_fixture_install` 1
— all passing. (The pre-existing clap-recursion stack-overflow test
`run_command_parses_explicit_state_bindings` exists on `dev` itself and is
excluded; not introduced here.)

**Live CLI smoke matrix: BLOCKED (infrastructure, not a defect) — tracked in #561.**

| Scenario | Result | Reason |
|---|---|---|
| `secret.K=prompt` (#555) | blocked | needs an *installed* app whose ledger declares the condition |
| `state.K=prompt` (#559/#560) | blocked | same |
| `port.main=3001` (#556) | blocked | same |
| `env.K=grant:<id>` (#558) | blocked | same |

Root cause (single, honest gap): `ato install` has no local-path mode, so the only
hermetic install is the mock-GitHub integration test, whose fixture declares none
of these conditions. There is no CLI-drivable hermetic install for a
launch-condition-declaring capsule. The four behaviors are therefore verified at
the library layer the PRs target (preflight probes, secret/env injection, state
materialization + resolve), but a true installed-relaunch e2e of the new
conditions needs the fixture in **#561**. No fake pass was recorded.

## Review status (not yet merged)
The four code PRs received REQUEST CHANGES — proof-boundary tightenings, all
addressed:
- #559: re-confirm the `state_binding_ref` proof (existence + install-profile +
  `condition_key`/`state_key`/status) **before** reading the target.
- #556: `port=auto` truly suppresses the env-`PORT` fallback (no concrete claim).
- #558: require the grant's `condition_key` to match `secret.K`/`env.K` (not just
  status + owner).
- #560: reject a `condition_key`/`state_key` mismatch in `resolve_state_binding_from_path`.

The integration + smoke results above predate these fixes and must be
re-confirmed before merge. Merge order once green: #559 → #556 → #558 → #560 →
this audit.

## Provenance
Batch coordinated 2026-06-06. PRs #556 (port), #558 (env-grant, +#557 follow-up),
#559 (state materialization), #560 (Desktop seam), plus this audit. Built in
manual worktrees off `origin/dev` (the batch `isolation:"worktree"` mechanism is
unreliable in this environment; non-isolated `bypassPermissions` background
agents on pre-made worktrees work). No `Co-Authored-By` trailers per repo policy.
