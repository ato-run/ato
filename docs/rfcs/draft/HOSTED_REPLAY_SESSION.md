# Hosted Replay Session v0

Status: Draft

## Identity and product contract

Computation is semantic identity. Record is evidence of evolution.
`ato.replay@1` is the existing Materialization that reconstructs one target
Computation from its ordered Record chain. A Replay Session is temporary
execution/control state around that Materialization; it is not a Computation,
Capsule, or Materialization. Replay does not create a new computation history.

The descriptor byte format, version, Materializer ID, and target identity stay
unchanged. Descriptor array order is the only application order. `observed_at`
may influence bounded presentation delay only; it never influences ordering or
identity.

Replay observes `C0 -> ... -> Cn`. Continue is a separate ordinary Hosted Open
from the Post's immutable `root_computation_ref == Cn`. A partial Replay cursor
is never an Open/Fork target. Future arbitrary-point continuation may use
`Record.head_after`, but is not part of v0.

## Built-in Adapter restore audit

`apply` is not intrinsically side-effect free. The physical Replay boundary,
not an Adapter name, supplies the safety policy.

| Adapter | attach | apply | filesystem | process | network / external service | secret dependency | observations | isolated Replay v0 |
|---|---|---|---|---|---|---|---|---|
| `ato.process@1` | spawns configured command with cleared environment plus explicit base/config environment | unsupported; process lifecycle is realized at attach | workload may mutate its cwd | starts an owned process group | workload may attempt arbitrary egress | configured environment could contain values, so Replay supplies none | process lifecycle capability only | allowed only inside `untrusted-v1`; process tree is killed on stop |
| `ato.pty@1` | spawns configured command and output readers; may emit Attach/initial Input | writes recorded Input to stdin; verifies recorded Output; other events are no-op | child may mutate workspace | starts child process | child may attempt egress | configured environment could contain values, so Replay supplies none | Attach/Input/Output and live gateway traffic | allowed only inside `untrusted-v1`, with ignored observations and view-only surface |
| `ato.workspace@1` | creates an in-memory session | applies Put/Delete/Rename under checked rooted-relative paths | mutates supplied workspace only | none | none | none | none during apply | allowed against the ephemeral Replay workspace only |
| `ato.binding@1` | decodes logical provider identity and emits Attach | validates recorded logical evidence; does not inject a value | none | none | none itself | a real Binding is deliberately absent; required Binding workload fails preflight | emits Attach at normal attach | descriptor/workload requiring Binding is rejected; no Binding Adapter instance is attached for credentials |
| `ato.http@1` | optional readiness request, binds listener | recorded Request performs a real TCP request to configured upstream; Response verifies the queued result | upstream may mutate workspace | none directly | can reach configured upstream and therefore is unsafe without a network namespace | recorded headers/body may contain sensitive data, so public progress excludes payload | proxy emits Request/Response | allowed only when the entire process is in `untrusted-v1`; upstream is reachable only inside that namespace |

## Hosted safety policy

A Hosted Replay runs only on a Runner advertising the Replay lease kind,
`execution_abi=process`, and `isolation=untrusted-v1`. The evaluator uses an
ephemeral session directory, a tmpfs root, minimal read-only runtime mounts,
`--unshare-all`, `--clearenv`, dropped capabilities, and no network namespace
bridge. It passes no recipient Binding grant and restores no creator secret.

Replay attaches with `IgnoreObservations`; no observation is committed, no
branch head is advanced, and no capture/share/fork route owns the session.
Only the audited built-ins are accepted. Required Bindings and unknown or
non-audited Adapters fail before `driver.begin`. Failure stops at the failing
Record, reports bounded metadata, terminates the sandbox, and discards its
physical directory.

The public progress envelope contains cursor/count, Record ID, Protocol/Port,
direction, and causal heads. It never contains payload bytes or resolved
payload content.
