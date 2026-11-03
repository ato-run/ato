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

The staging Studio source attempt still failed in Formation's static HTML
path. The Formation executor explicitly refuses native surfaces, and the
staging Runner was not advertising the required Pixel capability when
inspected. These are distinct from the successful direct Linux adapter probe.
End-to-end work still requires a supported Studio authoring/build/verification
route, independently reproducible observations, and an admitted, explicitly
enabled staging Runner, followed by an actual browser operation and receipt.
