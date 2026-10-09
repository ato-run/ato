# Linux isolation acceptance

`process_isolation.rs` runs the production Bubblewrap command builder and
Connected Runtime Worker's Landlock shim. It is deliberately ignored in the
ordinary unit suite and explicitly executed by the Linux process-isolation CI
job. It fails if the sandbox cannot start; absence of Bubblewrap or namespace
support is not counted as a passing result.

CI pins the dedicated namespace test to Ubuntu 22.04. Newer Ubuntu images can
restrict unprivileged user namespaces with AppArmor, preventing Bubblewrap
from starting. The job does not disable that protection globally or turn a
sandbox startup failure into a skip.

Use a dedicated Linux test host with Bubblewrap, Python 3 and the repository's
pinned Rust toolchain:

```sh
mkdir -p .tmp
export TMPDIR="$PWD/.tmp"
export ATO_RUNNER_ISOLATION_TEST_HOST=1
export ATO_TEST_PARENT_CREDENTIAL=synthetic-parent-credential-canary
cargo test --locked -p ato-connected-realization-worker --test process_isolation -- --ignored --test-threads=1 --nocapture
```

The tests create and remove only their own temporary directory. They use
synthetic host credentials, another tenant's State, an ambient credential and
a local HTTP management-service canary. They require read-only workspace input
and writable own State as a positive control.

There are two network modes. The isolated namespace must refuse the host
service. The direct-process host-network mode must reach it, making the missing
tenant network boundary explicit. The Worker therefore advertises
`isolation:runtime_launch=host-boundary-v1` and cannot authorize shared
runtime-launch placement. Its existing VM capability is independent.

These tests do not establish kernel escape resistance, OCI isolation,
Firecracker isolation, authenticated staging tenancy, or metadata policy.
They do not read real credentials or contact a real metadata service.
