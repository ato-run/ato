# Operations

Choose a reviewed binary and an explicitly authorized environment before using
a runbook. A procedure in Git does not prove it was executed on the deployed
host. Keep version, SHA/digest, active work, dependency versions, migration and
flag state with the operation record.

- [Formation worker build identity](formation-worker-build-identity.md): pair
  binary version/commit/profile with the deployment record.
- [Formation worker update and rollback](formation-worker-update-lifecycle.md):
  drain, quiesce, verify candidate, switch, journal/recovery and rollback.
- [Connected Runner rollout and rollback](runner-rollout-and-rollback.md):
  stop new assignment and confirm owned resources before switching binaries.
- [Component compatibility matrix](compatibility-matrix.md): which CLI, bundle,
  API/PWA contract and Runner launch-spec versions must agree (and how a
  capability string can lie about the binary behind it).
- [Runtime image ledger](runtime-image-ledger.md): record declared image and
  patch-candidate evidence; this is not an image scanner or security attestation.
- [Single JSX compiler provisioning](single-jsx-compiler-provisioning.md):
  provision and verify the pinned compiler used by Formation workers.

For enrollment and host diagnosis use the
[Connected Runner guide](../guides/connected-runner-self-host.md).
For the provider-specific security boundary use [Nacelle security policy](../../extensions/providers/nacelle/SECURITY.md).
Never include credential values, session cookies, signed capability URLs or
private application data in evidence committed to Git. External state is
rebound at runtime rather than captured as identity.

API/PWA deploys and remote migrations are owned by the corresponding service
repositories. Verify their deployment scripts, environment and applied
migration history; no API/PWA command is redefined here. Release automation
is in the [actual workflows](../../.github/workflows/) and governed by
[repository instructions](../../AGENTS.md), which already mark the old release
checklist unverified. Confirm that flow separately before a release.

Historical incidents, staging measurements and retired host provisioning are
in the [2026 archive](../archive/2026/README.md).
Raw [ops evidence](../ops/README.md) retains its original paths. The
[retention assessment](../archive/2026/documentation-reorganization/EVIDENCE-RETENTION.md)
explains how to externalize large logs without losing auditability.
