# Explicit Runtime Placement

Status: draft implementation contract
Date: 2026-10-24

## Decision

`--require-explicit-runtime-placement` (environment
`ATO_RUNNER_REQUIRE_EXPLICIT_RUNTIME_PLACEMENT`) is a default-off operator
restriction on a connected Runner. When enabled, the Runner advertises
`runtime_feature=explicit_runner_assignment_v1` and only advertises the
intersection of its actually supported lease kinds with `runtime_launch`.
An empty intersection stays empty. Other lease kinds have no explicit intent
wire in this version and are refused before any lease side effects.

The existing private `RuntimeLaunchLeaseCommand` envelope gains an optional
`explicit_runner_id`. It is Coordinator routing evidence, not authorization:
the API must derive it from current owner-authored explicit placement, retain
ownership/current Run/lease checks, and deliver it through existing authorized
lease claim. Automatic selectors must exclude marked Runners. The marker alone
never authorizes a Run, and a workload, Capsule or caller cannot supply this
field as a replacement for server-side placement checks.

If present, the value must be nonempty and exactly equal the local authenticated
Runner ID even when the operator restriction is off. Missing/null is accepted
only when the restriction is off. Existing lease validation and execution
restriction/authorization remain required. A common guard executes after lease
validation but before storage probing, preparing reports or lease directory
creation; the direct runtime handler repeats the same runtime guard before
materialization, bindings, journals or workloads.

## Compatibility and boundaries

Legacy envelopes omit the field and remain decodable. The private envelope is
currently deserialize-only; this change adds no serializer or new canonical
wire. Placement stays outside `launch_spec`, its digest, K, D and all IPC/Core
semantic types. Existing v1/v2/v3 bytes and digests remain unchanged. The flag
never enables Native, OCI, privileges, secrets, network, provider provisioning
or a different isolation policy. Default-off behavior retains existing lease
kinds and legacy payload handling. Runtime support is not inferred from the
marker.

## Verification

Tests cover default/CLI/environment configuration, marker advertisement,
lease-kind intersection including an empty set, legacy/null/wrong-type fields,
Some mismatch/empty refusal in either mode, matching explicit routing,
v1/v2/v3 digest independence, all non-runtime kinds, and both guarded execution
entries refusing before filesystem or API side effects. A test-only fixture
consumer accepts a server-produced existing claim envelope and performs only
parse, identity, assignment and launch-spec digest validation; it does not
execute a workload or grant authority.

Deployment/enablement and authenticated owner staging acceptance are separate.
No service installation, credentials, existing Runner flags, or paid resources
are changed by this implementation.
