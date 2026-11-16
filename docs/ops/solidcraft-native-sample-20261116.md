# SolidCraft 2-D native sample — pack evidence (2026-11-16)

Status: record of what was measured while adding `samples/solidcraft` (M2 PR5
preparation). Nothing here is a deploy or a publish. The sample is not run to
completion: the Contract's observation has no HTTP surface to observe on this
application (see §4).

## 1. Environment

- macOS 15 / aarch64 host, agent shell under a host sandbox.
- Repository: `ato-run/ato`, branch `feat/sample-solidcraft`, base
  `origin/dev` @ `759c34c46`.
- CLI: `ato 0.9.3`, built from this branch with `CARGO_TARGET_DIR=$PWD/target`
  (`cargo build -p ato-cli`).
- Upstream: `tangency-labs/solidcraft` v0.43.0, commit
  `8ba46e9e2181396a9b5903b98031399aef61cf26`.

## 2. Pack

```
$ ./target/debug/ato pack samples/solidcraft -o .tmp/solidcraft.capsule
Packed: .tmp/solidcraft.capsule
bundle_sha256=sha256:cf4291fb4adab4a17e3df0c08b8a28ff41387072081ccdc04085e20d13cb855d
contract_ref=sha256:fcbcfb8aa6a3e098464496a555f75e3bf347e378d9f52bc6b969f13412b86c90
derivation_ref=sha256:04cc3e7f8912903513db4e655e8c45f04c29b43d8aa81c05a28fd0f2cf83deb0
derivation_ref=sha256:5225d942cce5dd41776b40a5ffaf4caf83c831d82718d8b2b178a99cb0c6ce8b
```

- `04cc3e7f…` = `from-source` (build egress crates.io / index.crates.io /
  static.crates.io on 443, build phase).
- `5225d942…` = `prebuilt` (`download_bytes = 153719917`).
- Both reference the same `contract_ref`; the bundle file is 22,357 bytes.
- `bundle_sha256` and the DerivationRefs move with the workspace tree (it is
  part of every Derivation's input); `contract_ref` does not.

```
$ ./target/debug/ato run --list-derivations .tmp/solidcraft.capsule
{ "schema": "ato.derivation-candidates/1",
  "bundle_sha256": "sha256:cf4291fb…",
  "contract_ref": "sha256:fcbcfb8a…",
  "host": { "os": "macos", "arch": "aarch64" },
  "derivations": [ … two entries, one per D … ] }
```

Both entries report `runnable: false` on this host with
`native_sandbox_unavailable` (`sandbox-exec: sandbox_apply: Operation not
permitted`): the agent shell cannot apply the Seatbelt policy, so neither route
is launched here. That is an admission fact of this host, not a bundle defect.

## 3. Measured application values (upstream 0.43.0 binary)

```
$ solidcraft-cli --version
solidcraft-cli 0.43.0                       # exit 0, median 6.3 ms (5 runs)

$ solidcraft-cli run samples/solidcraft/box-measure.json --results
[ … two command results … ]                 # exit 0, median 12.3 ms
```

Only the `ms` timing fields differ between runs. The normalized transcript is
`samples/solidcraft/expected/box-measure.results.json`, 650 bytes,
`sha256:14f02d3346857dce809cfcb681ec5d0e88212c16cea4ed68d31e2d0327a8f8bc`
(identical across three consecutive runs).

Measured model: 40 × 30 × 20 mm box, `volume_mm3 = 24000.0`,
`area_mm2 = 5200.0`, `mass_g = 30.96` (Default material 1.29 g/cm³),
`bbox` 0..40 / 0..30 / 0..20, kernel 8 vertices, 12 edges, 6 faces, 1 shell,
closed. `eval` returns the same measurement at a median 14.0 ms.

## 4. Why the Contract is only a surface declaration

The profile binds `ato.contract.http@1`, `scenario@1` and `tls@1`; each of
them speaks HTTP to the route's `app.http` Port. Measured on the upstream CLI:

- `mcp --headless` is stdio JSON-RPC; `lsof -nP -iTCP` during a run lists no
  listening socket.
- `run`/`eval`/`exec`/`script`/`snapshot`/`render` are one-shot process
  commands; none opens a port.
- The upstream desktop app's control channel (`crates/control`) is JSON lines
  over TCP with a token, and rejects an HTTP request line.

So `samples/solidcraft/capsule.toml` pins the surface (`GET /` → 200) and pins
**no** body digest, because no body was ever observed. Neither D can satisfy
that observation until a terminal-exchange verifier exists or the application
gains an HTTP surface; a Run therefore stops at readiness and the receipt is not
`fully_satisfied`. The deterministic transcript above is the measured value
such a verifier would pin.

## 5. Integration gap closed to make pack possible

`ato pack` on the two-D sample failed before this branch:

```
error: compile portable Application from samples/solidcraft: portable application
profile violation: a native source build declares the build-phase network it requires
```

`apps/portable-application/src/lib.rs` requires a source route's Derivation to
state build-phase egress, but `ato.capsule/2` has no author-facing network
declaration (`Derivation` in `lib/formation/src/capsule_toml_v2.rs` is
`deny_unknown_fields` and has no network field), and
`build_dynamic_routes_bundle` previously left `requirements.network` empty for
every route. The builder now states the fixed crate-registry hosts for a source
route (`NATIVE_BUILD_EGRESS_HOSTS` members ending in `crates.io`, port 443,
build phase); a remote route declares nothing. Covered by
`an_authored_source_route_states_its_build_egress` in
`apps/portable-application/src/native_desktop_tests.rs`.

## 6. Verification run

```
cargo test -p ato-portable-application --lib native_desktop   # 75 passed
cargo clippy -p ato-portable-application --all-targets -- -D warnings  # clean
cargo fmt -p ato-portable-application -- --check               # clean
```

## 7. Not verified

- D1 fetch, digest check, zip unpack, signature check and launch on a host that
  can enforce the native policy.
- D2 consent, managed toolchain install, source digest, sandboxed build and
  cache reuse.
- Any K observation, and therefore `fully_satisfied`.
- Upstream's written position on name use and redistribution before publication.

## 8. Update 2026-11-16 (later): terminal K, declared argv, two terminal Routes

- `ato.contract.terminal@1` added for v2 native Routes (PR #1838), plus
  `ato.native.argv` for a bounded, directly-exec'd argument vector (PR #1840).
- Several terminal Routes may now realize one Contract when they declare the
  same terminal Port and operation set (PR #1842).
- Re-packed sample: `bundle_sha256=sha256:1f1895ecdcff6a3cedbdc381ea769a6effb70e38d4945897fde8e2afa268e4ee`
  (24,101 bytes), `contract_ref=sha256:34d52388678111292fa890603110c23b47ce51220f452a28c233a2259e174c98`,
  D1 `derivation_ref=sha256:80118875305df06ea1b43e8681ff0399c0408c492fcb89d84c85cc8afcb0db99`,
  D2 `derivation_ref=sha256:97d008d778db2ea77049aa915133f78e51ea1b7ab43f21864b0d854bb899ef76`.
- `--list-derivations` reports both Routes as `native_terminal`; both are
  `runnable=false` on this workstation only because it cannot apply the
  Seatbelt policy (`native_sandbox_unavailable`).


## 9. staging publish (2026-11-16)

- `ato hub publish`（dev CLI、staging session）で `capsules/ato/solidcraft` を公開。
  digest `sha256:1f1895ecdcff6a3cedbdc381ea769a6effb70e38d4945897fde8e2afa268e4ee`。
- API detail: `capability_surfaces=["native_terminal"]`、`derivations` 2 件（`sha256:80118875…` / `sha256:97d008d7…`）、
  `requires.platforms=["macos/aarch64"]`、`verification={state:"experimental", successful_runs:0}`。
- ページ https://stg-app.ato.run/capsules/ato/solidcraft は 200 で描画され、
  「ターミナル」「macos/aarch64」「実験的 / 成功した実行の報告はありません」を表示する。
  成功 Run が 0 なのは**実機未検証だから**であり、run count は成功 Run のみで数える方針どおり。
