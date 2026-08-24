# Hosted Browser Computation v1

## Decision

`ato.browser@1` remains the interaction Protocol and Adapter identifier.

The Protocol keeps the authoring vocabulary already used by generic Capsules:
the application endpoint is `server` and the Browser computation endpoint is
`controller`. `browser` is not a new role alias. This is intentionally
compatible with existing `ato.browser@1` Capsule declarations.
`ato.browser.computation@1` is the separate logical Semantics identifier.
Its residual contains only an interaction frontier. Origin/URL, Chrome profile
paths, DOM, cookies, localStorage, VM data, Record metadata, actor provenance,
and network identifiers are physical Binding or realization data and excluded.

For every accepted Browser event, `BrowserOperationIngress` performs:

```text
canonical Browser event
→ RunEvolutionAuthority derive
→ Browser live operation / ACK
→ Kernel commit + head
→ runner current-head CAS
→ one Record submission
```

`BrowserInputMode::ObserveAndApply` preserves the existing local CLI
observation path. Hosted Runs explicitly select `ApplyOnly`: the bridge does
not install trusted-event capture listeners and silently cannot create a
second Record from an actuator echo. Player can continue to use
`AttachedAdapter::apply(RecordEnvelope)`, while live Runner ingress uses the
new `AttachedAdapter::apply_operation(LiveOperation)` boundary and never
creates a fake persisted Record.

The Runner assigns a stable bounded `operation_id` before derivation. The
accepted operation context carries that id, event, transition and `run_seq` to
both the current-head CAS and Record submission. These values are operational
context, not Computation identity. A CAS failure retains exactly this context
for retry, fail-closes later input and capture, and does not replay the
physical Browser operation.

## Composition and lifecycle

Browser Semantics is registered for hosted Kernel use, but it is useful only
when an explicit Browser Computation/Port is present. Existing source and
`ato.authoring@1` computations are neither mutated nor reinterpreted. A later
composition assembles source and Browser children through `ComposeSemantics`.

The Activity-specific controller, participants, media, WebRTC, and credentials
from #1290 are not dependencies of this design. `ato-browser-host` owns the
generic private Chrome profile, loopback CDP, exact-origin navigation, bridge
injection/handshake, and bounded cleanup; Connected Worker does not depend on
`ato-cli`. Browser Materialization and all Browser state capture remain
intentionally absent.

## CI receipt

The P0-A `computation CLI` rerun for #1303, GitHub Actions run
`32702615368`, succeeded on Ubuntu, macOS, and Windows. The earlier Windows
`ConnectionRefused` in unchanged `ato-adapter-http` did not reproduce and is
recorded as flaky/infrastructure evidence, not as code-green history for the
original failed run.
