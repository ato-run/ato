# Component compatibility matrix

Status: operating reference
Date: 2026-10-28

Compatibility here means: a bundle that was accepted by one component can be
parsed, launched and verified by the others **without changing K or D**. It is
not a claim that every OS, runtime or state mode is supported everywhere; the
capability and OS tables below are part of the answer.

## Axes that must agree

| Axis | Where it is defined | What a mismatch looks like |
| --- | --- | --- |
| Capsule source schema | `ato.capsule/1` / `ato.capsule/2` in `capsule.toml` | `pack` refuses the directory |
| Portable bundle profile / wire | `ato.portable-application/1` = wire v3, `/2` = wire v4; computation-root = wire v2 | `bundle_index_invalid` / `unsupported profile` |
| Verification receipt | `ato.contract-verification-receipt/1` (bundle_sha256, contract_ref, derivation_ref, observations, fully_satisfied) | identity comparison refuses the receipt |
| PWA ↔ API contract | `PWA_API_CONTRACT` / `CURRENT_API_CONTRACT`; `426` fence | PWA refuses to call the API, or the API fences the PWA |
| CLI ↔ API auth | `/v1/auth/bridge` device code; `ato_dev_` credential (90 days) | `401 auth_required` / `invalid_device_credential` |
| Runner launch spec | `ato.runtime-launch-spec.v1` (incl. `StateAttachmentV1.backing`), `.v2` (service groups), `.v3` (Runner-local volumes) | `ATO_ERR_RUNTIME_LAUNCH_SPEC_*` at lease claim |
| API schema | Drizzle migrations + the Release Manifest pin | a query fails against a missing column |

## Runner binary preconditions

Advertising a capability is **necessary but not sufficient**: the binary must
also parse the spec that capability implies. A runner can advertise
`runtime_feature=runner_persistent_volume_v1` from its environment/service
configuration while its binary predates the spec field, and then reject the
lease at parse time.

| Launch spec feature | First commit that understands it | Capability | Failure if the binary is older |
| --- | --- | --- | --- |
| v1 process/OCI launch | pre-2026-08 | `runtime_launch`, `execution_abi=process|oci` | `ATO_ERR_RUNTIME_LAUNCH_SPEC_UNSUPPORTED_VERSION` |
| v1 `StateAttachmentV1.backing` (Runner-local volume request) | `a7c35792b` (2026-10-26, #1571 wire) | `runner_persistent_volume_v1` | `ATO_ERR_RUNTIME_LAUNCH_SPEC_FORBIDDEN_FIELD` (serde `deny_unknown_fields`) |
| v2 service groups | service-group lane commits | `oci_service_group_v1` | `ATO_ERR_RUNTIME_LAUNCH_SPEC_FORBIDDEN_FIELD` on a v1 parser |
| v3 Runner-local volume execution | v3 volume commits + `ATO_RUNNER_STATE_VOLUME_ROOT` | `runner_persistent_volume_v1` | `state_volume_runner_unavailable` (before claim) |

Before pointing a service at a new binary, check the commit rather than the
capability string:

```sh
git merge-base --is-ancestor a7c35792b <target-commit>   # v1 backing
```

2026-10-28 staging incident: the pinned candidate `0fabc1f7e` (2026-10-23)
was an ancestor of the backing commit, so every Memos (runner-volume) import
failed at verification with `interruptted` / `FORBIDDEN_FIELD` while the runner
advertised the volume capability. Rebuilding the worker from main and rolling
the six hosted slots fixed it; the bundle then verified and served HTTP 200.
See [Runner rollout and rollback](runner-rollout-and-rollback.md) for the
switch procedure and the rollback copy of the slot drop-ins.

## CLI / API / PWA current pins (2026-10-28)

| Component | Version / pin | Notes |
| --- | --- | --- |
| ato CLI | 0.8.1 | `dist plan --tag=v0.8.1` succeeds; cargo-dist requires tag == crate version |
| PWA contract | `PWA_API_CONTRACT=1` | served HTML carries the build stamp; stale bundles are fenced with 426 |
| API schema | migrations through `0377` on staging | production is behind; never infer from a commit number |
| Staging API | version `9da8c665-d39e-4444-950e-54bd7f0469bb` | `safe deploy a662ff565666` |
| Staging hosted Runner | `ato-connected-realization-worker-3b6a1df6e` | six slots; previous candidate `0fabc1f7e` is the rollback target |

## How to verify a combination

```sh
ato version --json
curl -s https://staging.api.ato.run/health
curl -s https://staging.api.ato.run/.well-known/capsule.json | head
```

For a PWA deploy, confirm the served HTML references the expected hashed
assets and the API origin it was built for. For a Runner, confirm the binary
commit and that the lease claim reaches `ready`, not just that the device
heartbeat is active.

## OS support (CLI and local Runtime)

| Feature | Linux | macOS | Windows |
| --- | --- | --- | --- |
| static web | supported | verified (arm64) | unverified |
| OCI | supported (native Docker) | rejected when an HTTP endpoint or service group is declared | expected rejected |
| process route | supported with `/opt/ato/toolchains` | conditional (Seatbelt); unverified | unverified |
| native surface / VM snapshot | Linux/KVM only | not supported | not supported |
| sandbox | Landlock | Seatbelt | not enforced (explicit) |
| release targets | aarch64/x86_64 gnu | arm64 + Intel (0.8.1+) | x86_64 MSVC only |

## Known gaps

- Runner binaries are not part of the cargo-dist release; the connected worker
  is installed with `cargo install --git` (no prebuilt Windows artifact).
- Windows sandbox is not enforced; do not claim isolation there.
- The PWA production build hides GitHub import behind
  `VITE_SUBMISSION_WIZARD_ENABLED=false`; the API submission wizard defaults
  off. Staging is the verified path today.
- API /health reports a static service version; deployment identity must be
  read from the release record, not from that endpoint.