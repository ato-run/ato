# #508 Close Audit — Installed-State DB, Resource Ledger & Relaunch Contract

_Status as of 2026-06-06 (`dev` @ `f4108a8`, the #546 merge)._

#508 has grown large. This audit fixes its current boundary: what is **done**,
what is **deferred** (with follow-up issues), the **MVP close conditions**, and
the **no-fake-proof** policy that governs the deferred work.

## MVP close conditions

#508 can close once these hold (they do today):

- **Installed-State DB is the device-local SOT** for installed launch conditions.
- **Relaunch admission reads the DB ledger**, not scattered lockfiles/manifests.
- **Storage / port / secret have real paths** (admission + claim + injection).
- **Secret prompt is a closed loop** for web/source relaunch.
- **Unsafe/incomplete state prompt is a typed not-implemented**, not faked.

The remaining items below are extensibility and recovery work that should not
block closing the contract; each has its own issue so progress stays readable.

## No-fake-proof policy (governs all follow-ups)

The ledger records **proof**, never **intent**:

- A ref/claim is written **only after** the real target/value exists
  (`secret_grant_ref` after the SecretStore write; `state_binding_ref` after the
  target store write). `prompt` is **not** proof.
- Raw secret values and raw host paths **never** enter `launch_condition_claims`,
  receipts, logs, or any cross-device index. They live only in local-private
  value stores (SecretStore; the new `state_binding_targets` table) and reach the
  process only at the spawn/container-creation boundary via a dedicated
  receipt-excluded `secret_env` channel.
- Non-interactive contexts that cannot satisfy a condition return a **typed
  error**, never a faked admission.

## Done (18 merged PRs)

| Checklist item | PR |
|---|---|
| InstalledStateDb schema + storage admission dry-run | #511 |
| Storage admission before download + claim on success | #512 |
| Port claims + logical-endpoint admission | #515 |
| Launch port admission decision/record core | #519 |
| Port-claim admission wired into installed web-service launch | #523 |
| Launch condition ledger as installed-app SOT | #527 |
| env/secret/state launch-condition extraction at install | #528 |
| Install-time port launch-condition extraction | #529 |
| Read ledger during installed-app relaunch preflight | #531 |
| Resolve launch conditions before relaunch admission | #532 |
| capsule:// launch-condition query + real secret/state resolvers | #534 |
| Restrict grant/binding registry condition-key kinds | #537 |
| Apply capsule query inputs before relaunch preflight | #539 |
| Wire capsule query inputs into installed relaunch entrypoint | #542 |
| Reject ambiguous capsule relaunch targets (ambiguity guard) | #543 |
| Parse secret/state `=prompt` and plan launch-condition prompts | #544 |
| Create secret grants from capsule prompt inputs (real creation) | #545 |
| Inject SecretStore-backed grants during installed relaunch (web/source) | #546 |

This closes, end-to-end:

```
secret.K=prompt
  -> SecretStore write -> secret_grant_refs -> relaunch preflight admission
  -> SecretStore read -> runtime env injection (installed web/source relaunch)
```

## Deferred (with follow-ups)

| Deferred item | Tracking | Why deferred |
|---|---|---|
| OCI executor secret injection | branch `feat/oci-secret-grant-injection` (in flight) | #546 scoped to web/source relaunch; OCI is a separate spawn boundary. |
| Manifest-free state-binding **target store** | branch `feat/manifest-free-state-binding-target-store` (in flight) | Prerequisite for real `state.*=prompt`; today's `ensure_registered_state_binding` is manifest-coupled. |
| `state.*=prompt` real creation (CLI/core) | #547 | Depends on the target store landing; writing a `state_binding_ref` without a real target would forge proof. |
| capsule:// `?port=` query → launch-time PortClaim | #548 | Port admission exists (#519/#515/#523); wiring the query inputs to it is distinct. |
| env-via-grant (`env.K=grant:<id>`) | #549 | Registry already accepts `env.*` (#537); planner/injection extension remains. |
| Materialized size reconciliation, ref-count & GC | #550 | DB indexes objects, but actual reconciliation/GC (pinned vs cache) is unimplemented. |
| Strict relaunch + repair / remap / re-placement | #551 | Preflight resolves conditions; typed failure-recovery paths are unimplemented. |
| Cross-device placement | #509 (open) | Reads device-local #508 data; out of #508's device-local scope. |

Desktop sibling: **#404** (Desktop UI path picker for explicit state binding) is
the desktop counterpart of the CLI/core `state.*=prompt` flow (#547).

## Recommendation

Close #508 once the two in-flight branches above merge (they complete the
"secret has a real path everywhere" and "state has a target store" guarantees),
and track the rest via #547–#551 and #509. The device-local installed-state
contract — SOT DB, ledger-driven relaunch admission, real storage/port/secret
paths, closed secret-prompt loop, typed not-implemented for unsafe state — is met.

## Provenance

Generated from a session audit cross-checked against `git log origin/dev` and
`gh pr list --state merged --search 508`. Code anchors: installed-state DB at
`crates/capsule-core/src/foundation/installed_state/` (db.rs ledger + grant/binding
refs); runtime injection at `crates/ato-cli/src/adapters/runtime/secret_injection.rs`.
