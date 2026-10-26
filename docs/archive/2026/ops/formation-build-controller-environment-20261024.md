# Formation build controller environment boundary

The build controller must clear its environment before starting bubblewrap,
in addition to bubblewrap clearing the eventual workload environment. Linux
bubblewrap keeps a PID-1 reaper inside its PID namespace. Its initial process
environment remains readable through `/proc/1/environ` even after `--clearenv`
changes the environment passed to the workload.

A CPU Linux probe on `ubuntu-sugamo`, using a public canary in
`ATO_FORMATION_TOKEN` only, reproduced the exposure with the previous outer
spawn: the workload found that canary in `/proc/1/environ`. Repeating the same
namespace/proc probe with an empty outer environment plus fixed PATH found no
canary. No staging credential was read, printed, or passed to this probe.

The production shim on this host applies Landlock, whose ptrace-domain check
also prevents reading the reaper environment: the unpatched full shim passed
that control. The existing runtime explicitly admits namespace-only execution
on kernels without Landlock. A controlled test shim that only execs the final
workload models that fallback; it must also be protected by the outer clear.

The common `run_step_with_variables` spawn now uses `env_clear()` and fixed
`/usr/local/bin:/usr/bin:/bin` PATH before exec. Every production build step
passes through `sandboxed_build_step_command` and this spawn. Declared step
variables still reach only the final workload, after namespace/Landlock setup.
The inner fixed environment, explicit mounts, network policy, build budget,
and declared authoring variables retain their existing behavior.

The regression test re-executes an isolated test process with two public
controller-token canaries and a fake private credentials file. An actual
untrusted build step scans its environment and every readable
`/proc/[0-9]*/environ`, checks that the controller's credentials file is absent,
and writes a proof artifact. It exercises both the production shim and the
controlled namespace-only shim, under denied and dependency-resolution network
policies. The packed workspace is checked for canary bytes.
No process-global test environment mutation is used.

Other inspected paths already clear the outer environment: ProcessAdapter
`spawn_with_group` and the owned OCI Adapter Docker client. OCI container
environment entries are explicitly constructed rather than copying the
Docker client's host environment. Build mounts
expose the verified source as `/src`, workspace as `/app`, optional cache as
`/cache`, fixed system/toolchain assets, and private namespace `/proc`; they do
not mount the controller home. Artifact packing walks the workspace and
rejects escaping symlink targets. These boundaries do not grant a repository
access to the controller credentials directory. Explicit user-secret Binding
values remain runtime inputs under their existing separate authorization.

This patch does not change any K, Derivation, source identity, claim API,
staging service, feature flag, or deployed binary. Installing or using the
new worker for an actual authorized job is a separate operational step.

The Linux-only source OCI caller also lacked the `user` initializer introduced
with the OCI Adapter's optional user field. `user: None` restores compilation
and preserves the prior behavior of honoring the image's declared user.

Validation on CPU Linux:

- Unpatched outer spawn plus the new regression: fails at
  `controller environment visible: /proc/1/environ` in namespace-only mode.
- Patched full Worker/Runtime test run: 29 suites, 553 passed, 3 existing
  ignored, 0 failures. This includes actual build/setup, static and process
  Formation, source-only upload/readback/abort, and denied-egress fixtures.
- Worker and Runtime all-target Clippy with `-D warnings`: passed on Linux
  and macOS; formatting and whitespace checks passed.
- The source-only fixture's delayed HTTP response now tolerates the expected
  closed-peer BrokenPipe/ConnectionReset; the cleanup request and confirmed
  abort remain asserted. Linux previously panicked in the fixture before it
  could observe cleanup.

The test configuration uses `TMPDIR` and `ATO_EXPLORATION_TEST_ROOT` under a
short owned workspace `.tmp/fe26` path. A long checkout-relative socket path
exceeds Linux's Unix-domain socket limit; this is a fixture path requirement,
not a changed runtime policy. All owned Rust/Cargo input mtimes were refreshed
before the final run: a shared Cargo target can otherwise reuse metadata from
a different checkout when rsync preserves older source mtimes. No shared
target files or another checkout's sources were deleted.
