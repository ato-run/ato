# SolidCraft (v0.43.0)

Two realizations of one Contract for [SolidCraft](https://github.com/tangency-labs/solidcraft),
the parametric 3D modeller by **Tangency Labs**. Ato is the **porter** here, not the
author: the application, its build and its name belong to Tangency Labs, and bugs in
the application belong upstream.

| | |
| --- | --- |
| Upstream | https://github.com/tangency-labs/solidcraft |
| Release | v0.43.0 |
| Commit | `8ba46e9e2181396a9b5903b98031399aef61cf26` |
| Licence | MIT or Apache-2.0 (upstream `LICENSE-APACHE`, `LICENSE-MIT`, `NOTICE`) |
| Brand | upstream `LICENSE-BRAND` (see below) |

## What this sample is

`capsule.toml` declares one Contract (K) and two Derivations (D) of the same
application:

- **D1 `prebuilt`** — the upstream v0.43.0 macOS universal CLI archive, referenced
  by URL, byte count and SHA-256 and fetched by the host. No artifact is inside the
  bundle.
- **D2 `from-source`** — the same CLI built from the pinned source commit with the
  pinned Rust toolchain, into the content-addressed build tree the Adapter
  materializes.

The two D share K and have different `DerivationRef`s. Selecting one removes
nothing from K, and neither D may fall back to the other.

## Licence and brand

The upstream code is dual-licensed **MIT or Apache-2.0**. This repository stores no
upstream file at all: D1 fetches the archive (which carries its own
`LICENSE-APACHE`, `LICENSE-MIT` and `NOTICE`) and D2 builds from the pinned
source tarball. Attribution and notices therefore travel with the upstream bytes,
not with this sample.

The upstream `LICENSE-BRAND` keeps the **name, logo, icons, look and theme** of
SolidCraft with Tangency Labs, and requires written permission to distribute a
modified SolidCraft under that name or to use the marks to promote something else.
For that reason this sample **ships no application binary, icon, logo, screenshot or
theme**, and the bundle contains none. The Application title
(`SolidCraft v0.43.0 - Tangency Labs, Ato port`) is textual attribution only. Do
not add brand assets to this directory.

## The Contract (K)

```toml
[[contract.observation]]
id = "surface-root"
use = "ato.contract.http@1"
port = "app.http"
method = "GET"
path = "/"
status = 200
```

K states the surface the route must own: its declared `app.http` Port answers
`GET /` with `200`. **No `body_digest` is pinned**, because no response body has
ever been observed for this application — see the next section. A digest that was
never measured would be a claim, not evidence.

### Why K is only a surface declaration

SolidCraft's CLI is a one-shot command-line program. Measured on the upstream
0.43.0 binary:

- `solidcraft-cli --version` → `solidcraft-cli 0.43.0`, exit 0.
- `solidcraft-cli mcp --headless` is a **stdio** JSON-RPC server; `lsof -nP -iTCP`
  while it runs lists **no listening socket**.
- `run`, `eval`, `exec`, `script`, `snapshot` and `render` are one-shot process
  commands; none opens a port.
- The only loopback server upstream has belongs to the **desktop app**, not the CLI:
  `crates/control` speaks JSON lines over TCP behind a token, and it rejects an
  HTTP request line outright.

The `ato.contract` verifiers this profile binds are `http@1`, `scenario@1` and
`tls@1` — all of which speak HTTP to the route's Port. None of them can observe a
program that answers on stdout. So this sample can pin the surface, but neither D
can satisfy that observation today: a Run stops at readiness, and the receipt is
not `fully_satisfied`. This is recorded, not hidden.

What *is* deterministic and measured is the CLI's transcript, which is what a
terminal-exchange verifier would pin. The values below are the ones K should carry
when that verifier exists:

```
$ solidcraft-cli run samples/solidcraft/box-measure.json --results
exit code: 0
stdout (minus the per-command `ms` timings) is byte-identical across runs
normalized transcript: expected/box-measure.results.json, 650 bytes
sha256 of that file: 14f02d3346857dce809cfcb681ec5d0e88212c16cea4ed68d31e2d0327a8f8bc
```

The measured design is a 40 × 30 × 20 mm box:

| value | measured |
| --- | --- |
| `volume_mm3` | 24000.0 |
| `area_mm2` | 5200.0 |
| `mass_g` | 30.96 (Default material, 1.29 g/cm³) |
| `bbox` | min [0,0,0], max [40,30,20] |
| kernel | 8 vertices, 12 edges, 6 faces, 1 shell, closed |
| exit code | 0 |

Timings from five runs on the M1 host (unstable by nature, so not in K):
`--version` median 6.3 ms, `run … --results` median 12.3 ms, `eval …` median
14.0 ms.

To reproduce the normalized transcript:

```sh
solidcraft-cli run samples/solidcraft/box-measure.json --results | python3 -c '
import json,sys
d = json.load(sys.stdin)
for step in d: step.pop("ms", None)
sys.stdout.write(json.dumps(d, sort_keys=True, separators=(",", ":")) + "\n")' \
  | shasum -a 256
```

## D1 — prebuilt (upstream reference, no redistribution)

- URL: `https://github.com/tangency-labs/solidcraft/releases/download/v0.43.0/solidcraft-cli-0.43.0-macos-universal.zip`
- Bytes: **153,719,917**
- SHA-256: `0633d662f3b410da90616d6c7e412f6d799693164541abea82c8d53802ad1d0a`
- Format `zip`, `macos`/`aarch64`, entry
  `solidcraft-cli-0.43.0-macos-universal/solidcraft-cli`
- Signature policy: the default `require-valid`. The upstream macOS binary is signed
  with a Developer ID (Brennen Herbruck, `G5QA5U5X2S`) and notarized.

```sh
ato run .tmp/solidcraft.capsule --list-derivations
ato run .tmp/solidcraft.capsule --derivation <D1 derivation_ref> \
  --verification-receipt d1-receipt.json
```

The Run fetches the archive with a streamed hash and the declared size, verifies the
digest, unpacks the zip, verifies the signature, launches the entry and only then
looks for the surface. A deleted asset is `native_fetch_not_found`; changed bytes
are `native_fetch_digest_mismatch`; a changed size is
`native_fetch_size_mismatch`; an unsigned or ad-hoc binary under `require-valid` is
`native_signature_policy_unsatisfied`.

## D2 — from source

- Source URL: `https://codeload.github.com/tangency-labs/solidcraft/tar.gz/8ba46e9e2181396a9b5903b98031399aef61cf26`
  (GitHub's commit tarball for `https://github.com/tangency-labs/solidcraft` at the
  pinned commit; the plain repository URL serves HTML, so it cannot be the object
  whose digest is pinned.)
- Source bytes: **217,936,976**, SHA-256
  `1d0e68cd22e86e225123fa55825e8b91f5efcac379d98e38669ee709353cb8e3`
- `Cargo.lock` in that tarball: 205,494 bytes, SHA-256
  `b35566c1e58ca752da5669fdcca6951df3c2e0b514d4019a7b02426414804743`
- Toolchain: `rust:1.95.0`, from the workspace's `rust-version = "1.95"` (the oldest
  release the code builds on; `rust-toolchain.toml` pins 1.99.1 for development, but
  the route uses the declared minimum).
- Build: `cargo build --locked --release -p solidcraft-cli`, `content-hash` cache.
  `build_argv` is the fixed cargo vocabulary — there is no shell and no arbitrary
  argument.
- Entry: `.ato-build/target/release/solidcraft-cli`
- Build egress: the builder states it from the Adapter's fixed host set —
  `crates.io:443`, `index.crates.io:443`, `static.crates.io:443` (build phase
  only). The source archive itself is fetched before the sandboxed build.

```sh
ato run .tmp/solidcraft.capsule --derivation <D2 derivation_ref> \
  --allow-native-source-build --verification-receipt d2-receipt.json
```

The first Run asks for consent before installing the managed toolchain under
`$ATO_HOME/toolchains/rust/1.95.0-<target>` (no `sudo`, no `PATH` change,
child-scoped `CARGO_HOME`/`RUSTUP_HOME`), then fetches, verifies the source digest
and the commit directory, checks `Cargo.lock`, builds in the Seatbelt sandbox and
launches. A second Run reuses `$ATO_HOME/cache/native-build/<hash>` and reports
`build_cache: hit` without building. A tarball whose commit directory does not
match is `native_build_source_commit_mismatch`; a `Cargo.lock` that differs is
refused before building.

## Reading a receipt

Both receipts must carry the same `contract_ref` and different `derivation_ref`s.
The fields that matter:

- `bundle_sha256` — transport equality of the file that was run.
- `contract_ref` — K's identity; identical for D1 and D2, and never rewritten by a
  selection, a failure or a retry.
- `derivation_ref` — the D this Run realized.
- `realization` — `native_desktop` (Hub metadata names the route
  `native_pixel`).
- Signature verdicts (`codesign --verify --deep --strict`, `spctl --assess`),
  the materialization sentence for the artifact, and `build_network` for D2.
- The observation evidence and `fully_satisfied`. A successful build is **not** K
  satisfaction: only the common verifier decides that, from the observations it
  ran. With K as declared today it stays false (see above).

The receipt is evidence of one Run. It is not the Capsule, and its PIDs, paths,
endpoints and attempt ids are not part of K or D.

## What was verified on this branch, and what was not

Verified locally (macOS, `ato` built from this branch):

- The deterministic CLI measurements in the tables above, on the upstream 0.43.0
  binary.
- `ato pack samples/solidcraft -o .tmp/solidcraft.capsule` succeeds with two
  Derivations over one `contract_ref`. The exact refs depend on the workspace
  tree, which is part of each Derivation's input, so they are recorded per build
  in `docs/ops/solidcraft-sample-20261116.md` rather than in this file. K's
  `contract_ref` does not move when the workspace or the D set changes.
- `ato run --list-derivations` prints both candidates over that one
  `contract_ref`, including D1's `download_bytes = 153719917` and D2's build
  egress.

Not verified (no run on a host that can enforce the native policy):

- D1's fetch, digest check, zip unpack, signature check and launch.
- D2's consent, toolchain install, source digest, sandboxed build and cache reuse.
- Any K observation. The development host used here reports
  `native_sandbox_unavailable` (`sandbox_apply: Operation not permitted`), so the
  route is not launched at all, and the CLI answers on stdout rather than on
  `app.http` in any case.
- Notarization, stapling and revocation are outside the Adapter's scope; only
  `codesign`/`spctl` on the materialized artifact are checked.
- Upstream's written position on name use and redistribution (`LICENSE-BRAND`)
  should be confirmed with its maintainers before this sample is published anywhere.
