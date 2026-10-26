# Native Terminal connected lease dispatch — 2026-10-24

> Status: historical evidence / past plan. Language: en.
> Preserved content; relative navigation updated. Current [documentation map](../../../README.md).

This child connects the owned Terminal primitive to the existing lease evaluator.
Shared Runner deployment and the API viewer-access rollout remain disabled.

## Actual execution boundary

A fake API supplied authenticated control/workspace/status transport, while the
real `execute_lease` entry performed envelope validation, storage admission,
preparing report, lease owner marker, immutable workspace materialization,
PreparedRun resolution, OCI launch, PTY exchange, common Native capture, scoped
proxy publication, stop, existing finish gate, stopped acknowledgement and
journal/lease-directory removal. Docker, HTTP/WS ingress, kernel principal,
Terminal input/resize and resource cleanup were real CPU Linux execution.

The private probe returned the actual `go\r\n1\r\nEND` bytes only with a correct
Run/lease/Runner/Port/path observation assertion. No proof and a wrong Port
returned 403. The existing Rust K verifier accepted those bytes against the
unchanged authored K. Only then did the fake API issue a browser assertion;
viewer input returned `2`, and repeated GET still returned frozen `1`.
The actual API counterpart must enforce the same current-attempt receipt gate;
this fixture is not a staging/publication or external-client acceptance claim.

Native disabled, missing key and weak key were rejected before workspace fetch.
A second real Docker test cancelled startup after its owned creation marker
existed: the original StartupCancelled classification and confirmed cleanup
were preserved, no ready Surface was returned and no owned resources remained.
The first actual test exposed reqwest's blocking-runtime panic when invoked
inside an async context. The bridge now uses block_in_place for the same
existing synchronous control callback; the regression exercises it directly.

## Verification

- Linux connected Runner library: 154 passed, 9 opt-in cases ignored by default.
- All six actual Docker Native cases explicitly executed: two connected-dispatch
  cases and four physical-lifecycle cases. This gives 160 distinct Runner tests
  passed; the three other existing opt-in Runner cases remain unexecuted.
- Legacy Web/process/OCI, scoped HTTP/observation ingress, execution authorization,
  state commit/quarantine and recovery regressions are in the passing suite.
- Bounded real stalled-HTTP control/renewal test passed; neither request waits for
  the existing default 60-second client timeout.
- All-target locked clippy, architecture-check (44 packages), format and diff
  checks passed. The actual worker test shim was built in the isolated target;
  no installed service binary was changed.

Actual CPU tests used the explicitly selected already-present pinned Python
image, with no pull, GPU, paid provider, operator key or production operation.
Fresh temporary roots remained under the workspace. The isolated host logs are
under `.tmp/dispatch.R4NbgO` (actual Native cases) and
`.tmp/dispatch-reg.sLqJ7Q` (154-regression/clippy/architecture) on the test host.

## Rollout prerequisites

1. Install compatible API report/session metadata and the same-Run/lease completed
   verification-job plus exact accepted-import-receipt access gate. Private K
   observation must remain available while browser access is pending.
2. Preserve the Rust report's per-D descriptor and Ports; require client Terminal
   acceptance and Runner Native-feature × Terminal-profile × Linux/OCI matching.
3. Supply trusted server HTTPS origin, current Run session/version and an explicit
   finite duration. The existing per-Runner claim key is mandatory; no global
   Surface keyring is installed or serialized into a command.
4. Verify the actual API/Runner/PWA combination, K failure/late receipt/cancellation,
   reconnect, save/restore and orphan recovery on an isolated staging allocation.
5. Only after those checks, explicitly opt in the intended Runner. The source flag
   defaults to false and was not changed on any shared Runner here.

GUI/Pixel remains a subsequent owned-producer slice. No Pixel capability is
advertised. Native mutable-network/Broker grants and legacy declared guest TCP
listeners remain outside this initial host-owned Terminal backend.
