# Geany native Pixel runner probe (2026-11-03)

This is a physical adapter investigation, not a Studio acceptance or a
verification receipt. No shared staging deployment, flag or migration was
changed.

The application was built from GitHub `geany/geany` commit
`0e02cc52a7634d1fed8511bd44730ea0ab791d9f`. The build context contained that
source and a separately authored Dockerfile; it was built through the isolated
source-OCI builder with its explicit network gate. The app has no VNC server.
The built app image was
`ato-source/afd3c22b6d2e@sha256:b4ff17ab8c8c58f36644c35826b09396724c765994eba1eaa8a2837322e575ce`
and the OCI archive SHA-256 was
`2924016ad35d8d3e8f98dbbe61dbcd9c1352c9ef53eee95f7111ea6fbd88b40c`.

A separate Runner-owned display helper was built from
`tools/native-display-helper/native-display` and TigerVNC. Its image was
`ato-source/96366216222f@sha256:b30c92090d1c1fac7209cfe77beb32d0c8631627e47f283848d9ad03a58e9037`;
its tar was 111,056,896 bytes and its `PortableOciArchive` JSON was 148,076,007
bytes, below the 256 MiB configuration limit. On the same Linux host,
`actual_local_parent_and_warm_helper_preflight_refuses_aggregate_overcommit`
passed with this exact image and archive, checking the existing local cgroup,
the warm helper image and aggregate-resource refusal. This local preflight does
not establish staging registration or readiness.

On an isolated Linux test root, the ignored
`actual_geany_pixel_frame_changes_after_keyboard_input` test started Geany
with `HOME=/tmp`, opened an empty workspace file, sent `g` via the Native
Pixel probe and `h` through the private RFB relay, captured a frame, stopped the
run and checked resource cleanup. Manual inspection of the captured frame
confirmed `gh` in the Geany editor. The test records frame changes; the visual
inspection is the evidence for the entered characters. It does not construct a
Contract or produce a receipt.

Two runs with the same app and helper produced distinct observations:

| Run | After first key | After second key |
| --- | --- | --- |
| 1 | `554da8fc8112601b5189ee5f57f92570ab5f66d607f60461db260cb1d086b35c` | `001d72a0ef43539361c01218247c7b58d6293abcf496a652cfc299d409d7a7ea` |
| 2 | `165b588bf51ad20c5e81e42589371ae626e0ea4fad12105a7c65fed3b3a5f63b` | `5c8e497daf399fa49ed16736eb6cbf58a1959afcf5fcf8f3b5c25628c7d30796` |

Geany displays timestamps in the captured window. Full-frame digest equality
is therefore not established across runs with that configuration. An
exploratory variation adding Geany's `--no-msgwin` produced the same first-key
digest (`763deb416a7116074edd6493142881f581e64728ce9ffe2423bf23b6d33a3139`)
and second-key digest (`2a78ee2796bb0e67dc2ea17ee7eda7cd6b3e77b89a6d6d02e1c65a0a18fc922e`)
in two consecutive runs on the same host. This is a useful lead, not a
cross-environment reproducibility guarantee or a verified Contract. A measured
frame cannot be copied into K during the same execution: the independently
authored contract must remain unchanged and the verifier must evaluate real
observations.

On 2026-11-04 a new direct Linux adapter attempt used the independently
recorded first-key body above as an input to K *before* launching Geany. The
runtime verifier evaluated the actual private-probe response against that
unchanged K and returned all observations satisfied. Its ContractRef was
`sha256:0118d5aab2d8e3bb86e87a627a3a0405ff675e006862b6cd0c337efecf4df442`.
The same verifier rejected both a substitute readiness body and the correct
body on the wrong Port. This was a local adapter verification, with no
Formation, Hosted Run, browser session or hosted verification receipt.

A follow-up direct adapter attempt used the validated portable bundle
`sha256:3b1c9fee71b9e664780a29d7381f58e27a3fb5fe860ad4e9ba033ba42eaa2705`,
including its original ContractRef above and DerivationRef
`sha256:0dc98f01b28bdc159adcb0b0223d80ea23c5a9314ae62a6741c48a3ab9723aa7`.
The Runner test materialized the bundle's empty `main.py` workspace and used
the bundle's K and D without changing them. The actual first-key observation
satisfied K; an unrelated body and a wrong Port were rejected. The authored
input is [the fixture](fixtures/native-geany-linux-amd64-20261104.toml), built
with the experimental `__native-bundle` command and an empty `main.py`. The
image reference identifies a locally held, digest-pinned image; this does not
make the image available to a registered staging Runner. The ignored test takes
the bundle via `ATO_NATIVE_PIXEL_TEST_BUNDLE` in addition to the explicit app
and helper archive paths. This remains a direct adapter test, not a Formation
or hosted attempt.

The direct adapter test was repeated after adding the shared native launch
projection. It started the same immutable Geany bundle using the generated
`RuntimeLaunchSpecV1` and again satisfied the unchanged K. Wrong body and wrong
Port remained rejected and the owned resources were stopped. This validates
the projection through a real local adapter, not the Formation frontend or a
registered staging Runner.

A subsequent Linux test ran the fixed portable Geany bundle through the
dedicated native realizer and the common Formation attempt, using the trusted
helper and immutable application archive. The Rust-issued FormationRuntime
receipt named K `sha256:0118d5aab2d8e3bb86e87a627a3a0405ff675e006862b6cd0c337efecf4df442`
and D `sha256:0dc98f01b28bdc159adcb0b0223d80ea23c5a9314ae62a6741c48a3ab9723aa7`;
all observations were satisfied, the attempt record finished, cleanup was
confirmed, and the Runner-owned journal contained no leftover entry. The test
ran locally on Linux, not through the Studio or staging control plane.

The source selection was then changed to the unmodified `src` subtree of the
same GitHub commit. The full repository contains a safe relative symlink which
the portable tree currently refuses, so this test does not claim to package
the repository root. The previous K failed twice: the first-key frame was
`1f842ea46ef5cb0a877471d4fa7eb346ff8b221d85d6ab3bc3a19f227f01b72b`,
whose probe response body hashes to
`7b8ab639863f5b895e7656b186a9665bb92c51f88df46d2a04570c19a12c37ce`.
After those independent observations, the [source fixture](fixtures/native-geany-source-linux-amd64-20261104.toml)
fixed that body digest before a new attempt. The resulting bundle was
`sha256:12c90761ce8cb75cf43f2997c662bce33234caeda6f9f76349766ca83a72a86d`.
The separate common Formation attempt produced a fully satisfied receipt for K
`sha256:dba72da3da110f31dd551a046a69cd8c50ae53bb2aba5b02f1bd00dd2b89e395`
and D `sha256:84cc0aefc3e05d888e9ba7bed711e55f51f593ae135bd9f8ccfdfc82391caa3d`,
finished its attempt record, and confirmed cleanup. This is a host-specific
frame digest, not a claim of cross-host reproducibility or staging acceptance.

An additional Linux integration test ran the same source fixture through
`run_claimed_job`: source extraction, the v1 native authoring route, portable
bundle construction, native Formation attempt, Rust receipt, and result
publication. The API was a local test fixture, not the staging API. The
published result retained the K and D above, `fully_satisfied=true`, and the
current attempt ID. The test also checked the bundle upload occurred. Two
earlier trials were stopped during an unacceptably slow archive read, before
the attempt record advanced from `not_started`; the final trial used a bounded,
pre-admitted buffered read. Its physical attempt finished and cleaned up.

Cross-repository validation of the first real result exposed an internal
`lease_id` in its Formation receipt, which the API correctly refused as an
execution-only field. After removing that field from Formation evidence, a new
real Worker job with the same fixed K and D passed the API's existing
`validateFormationResultV2` unchanged. This checks the result wire contract;
it does not exercise staging admission, registration, or hosted execution.
The native OCI path records its actual internal-network isolation rather than
the process sandbox's bubblewrap/Landlock description. An internal Docker
bridge alone does not establish that a container cannot reach host services;
private-isolated native Formation therefore initially refused admission until
that host boundary was enforced and tested.

A subsequent isolated Linux adapter run used separate Docker `none` network
namespaces for the exact Geany image and helper. Docker inspection confirmed
that each had no IP address or gateway; a host listener was reachable from the
host, but connection attempts from each container failed. The same run observed
the original fixed Contract as satisfied and confirmed cleanup. The worker now
records `oci-network-none;native-pixel` for this execution. Separately, a
Linux worker test inside a temporary systemd slice confirmed that a bounded
common parent is accepted and the root or unrelated system slice is refused;
the test slice and its temporary limits were removed afterward. Private native
Formation now checks that common boundary before archive loading and before
realization. The later bounded private attempt is recorded below. These tests did
not exercise Studio, staging or a browser.

The `actual_geany_owned_lease_uses_verified_application_archive` Linux test
then exercised `NativeLeaseRun` itself with the same bundle and its projected
launch spec, the exact Geany OCI archive, and a helper prepared through the
normal local cgroup and warm-image admission checks. The private observation
satisfied the unchanged K, and stopping the lease confirmed resource cleanup.
No staging lease, hosted receipt or browser session was involved.

The current staging Runner only reports its package version, so that value
cannot identify its deployed commit or build. A candidate change adds optional
semver build metadata to `agent_version` when `ATO_BUILD_COMMIT` is a canonical
40-character commit and `ATO_BUILD_DIRTY=clean`; an optional alphanumeric
`ATO_BUILD_ID` can identify the build within the API's length limit. This is a
build-supplied claim, not an attestation of the binary. A deployment must still
verify the clean source, actual executable digest, and the API-reported value.
An absent or invalid commit, a dirty/unknown build, or oversized metadata keeps
the legacy version unchanged. This capability is not deployed to staging.

The staging Studio source attempt still failed in Formation's static HTML
path. The Formation executor explicitly refuses native surfaces, and the
staging Runner was not advertising the required Pixel capability when
inspected. These are distinct from the successful direct Linux adapter probe.
End-to-end work still requires a supported Studio authoring/build/verification
route, independently reproducible observations, and an admitted, explicitly
enabled staging Runner, followed by an actual browser operation and receipt.

## Bounded private Formation, 2026-11-04

A later complete `run_claimed_job` used private admission, the same pinned
Geany GitHub source subtree and the same K fixed above. The Worker, Geany and
display helper shared a temporary systemd cgroup parent limited to 1 GiB,
256 MiB swap, 128 tasks and one CPU. Both containers used Docker network
`none`. The separate image and resource limits were fixed before the attempt
in [the private fixture](fixtures/native-geany-private-linux-amd64-20261104.toml).
The image was built from the same Geany source, with debug symbols removed;
the reduced root filesystem retains the GTK MIME database and the Adwaita
cursor-theme link. Missing MIME data previously caused Geany to exit, while a
missing cursor theme produced an honest fixed-contract digest mismatch. No
measured value was substituted into K.

The prebuilt app archive was
`ato-source/116cc9ddb184@sha256:c45826486683df6e27159db11fc4ba9a5f08e4d9f6079cf881814b530ad7e03b`;
the separate display helper was
`ato-source/96366216222f@sha256:b30c92090d1c1fac7209cfe77beb32d0c8631627e47f283848d9ad03a58e9037`.
The resulting bundle SHA-256 was
`sha256:c1a3f0a488cf9be1dde136069ca97e3c840ae3ad13da74a70b670fad49a9a2e0`,
with K `sha256:dba72da3da110f31dd551a046a69cd8c50ae53bb2aba5b02f1bd00dd2b89e395`
and D `sha256:730ea4f724a59530f698cad524977344b0bb0d674fcd9351b5b3d538b8f7304d`.
The actual Worker published a receipt with `fully_satisfied=true` tied to the
Formation attempt, after cleanup; the maximum reported slice memory was
639.8 MiB. The temporary service and slice were inactive after the test. The
actual result also passes the API's result validator. The API was a local test
fixture: this is not a staging Runner, hosted receipt, or browser operation.
