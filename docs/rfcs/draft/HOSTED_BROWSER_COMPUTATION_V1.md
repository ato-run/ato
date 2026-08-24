# Hosted Browser Computation v1

## Decision

`ato.browser@1` remains the interaction Protocol and Adapter identifier.
`ato.browser.computation@1` is the separate logical Semantics identifier.
Its residual contains only an exact expected origin and an interaction
frontier. Chrome profile paths, DOM, cookies, localStorage, VM data, Record
metadata, actor provenance, and network identifiers are excluded.

For every accepted Browser event, `BrowserOperationIngress` performs:

```text
canonical Browser event
→ RunEvolutionAuthority derive
→ Browser live operation / ACK
→ Kernel commit + head
→ runner current-head CAS
→ one Record submission
```

The Browser bridge accepts only trusted local input events for observation.
Its own synthetic actuator events are not observed back as Records. Player can
continue to use `AttachedAdapter::apply(RecordEnvelope)`, while live Runner
ingress uses the new `AttachedAdapter::apply_operation(LiveOperation)` boundary
and never creates a fake persisted Record.

## Composition and lifecycle

Browser Semantics is registered for hosted Kernel use, but it is useful only
when an explicit Browser Computation/Port is present. Existing source and
`ato.authoring@1` computations are neither mutated nor reinterpreted. A later
composition assembles source and Browser children through `ComposeSemantics`.

The Activity-specific controller, participants, media, WebRTC, and credentials
from #1290 are not dependencies of this design. Its generic Chrome/profile/CDP
host is the extraction source for the next implementation slice; Connected
Worker must not depend on `ato-cli`. Browser Materialization and all Browser
state capture remain intentionally absent.

## CI receipt

The P0-A `computation CLI` rerun for #1303, GitHub Actions run
`32702615368`, succeeded on Ubuntu, macOS, and Windows. The earlier Windows
`ConnectionRefused` in unchanged `ato-adapter-http` did not reproduce and is
recorded as flaky/infrastructure evidence, not as code-green history for the
original failed run.
