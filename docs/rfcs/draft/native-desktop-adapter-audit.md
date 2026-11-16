# ato.native@1 Adapter audit (M2 / PR3)

Status: Draft (2026-11-16). Target: the native desktop D implementation of `ato run`.
Contract: workspace `docs/rfcs/draft/capsule-hub-m2-native.md` section 2.

This document is an audit record. It separates what is *declared*, what is
*implemented*, and what is *verified on a real host*. Nothing unimplemented is
reported as complete.

## 1. Audited layers

| Layer | Location |
| --- | --- |
| descriptor (declaration validation) | `lib/formation/src/native_desktop.rs` |
| profile validation and closure | `apps/portable-application/src/lib.rs` (`validate_native_desktop_route`) |
| acquisition (D1 remote / D2 source) | `apps/portable-application/src/native_desktop/{remote,source_build}.rs` |
| managed toolchain | `apps/portable-application/src/native_desktop/toolchain.rs` |
| unpacking | `apps/portable-application/src/native_desktop/artifact.rs` |
| signature, sandbox, launch, teardown | `apps/portable-application/src/native_desktop/macos.rs` |

## 2. Added by PR3 (implemented, unit/integration tested)

- Exclusive artifact source declaration: `bundled`, `remote`, `source`. Two of
  them, a partial one, or none are refused with
  `native_artifact_source_ambiguous`, `native_remote_incomplete`,
  `native_source_incomplete` and `native_artifact_source_missing`.
- D1: `ato.native.remote_url` / `remote_sha256` / `remote_bytes`. https only, no
  fragment, 4 GiB bound. The fetch streams through a hashing, size-capped
  writer, writes a temporary file, verifies, then renames. A missing asset
  (`native_fetch_not_found`) is a different failure from changed bytes
  (`native_fetch_digest_mismatch`), and a wrong size is a third
  (`native_fetch_size_mismatch`).
- D2: `ato.native.source_url` / `source_commit` / `source_sha256` /
  `cargo_lock` / `toolchain` / `build_argv` / `build_cache`. `build_argv` is the
  fixed `cargo build --locked --release [-p <pkg>]` vocabulary; there is no
  shell and no arbitrary token.
- Acquire, unpack, verify the signature and launch are separate stages, and
  each names its own failure. An unpacks-unknown format is not reported as a
  corrupted artifact (`native_format_extractor_unavailable` vs
  `native_artifact_invalid`).
- Managed toolchain: not installed and not approved stops with
  `native_toolchain_consent_required`; a non-interactive Run without approval
  stops with `native_toolchain_consent_unavailable`. Only after approval is it
  installed under `$ATO_HOME/toolchains/rust/<version>-<target>`, with no
  `sudo`, no PATH or shell-profile change (`--no-modify-path`), and child-scoped
  `CARGO_HOME` / `RUSTUP_HOME`.
- Isolated build: the existing Seatbelt profile makes the source read-only and
  allows writes only to the Run-scoped target directory, scratch and cargo
  caches; TCP egress is limited to 443. The child environment is cleared and
  then set explicitly.
- Output cache: `sha256(recipe version, source_sha256, commit, cargo_lock,
  toolchain, target, build_argv, entry)` under
  `$ATO_HOME/cache/native-build/<hash>`. A second Run reuses it without
  building; a cached file whose bytes no longer match its manifest digest is
  discarded and rebuilt.
- Build success is not K satisfaction: the build is one realization stage, and
  the common verifier alone decides `fully_satisfied` from its observations.
  This PR does not change that path.

## 3. Not implemented (recorded as incomplete)

| Item | Current state | Refusal |
| --- | --- | --- |
| `zip` artifact unpacking | accepted in the declaration vocabulary (remote only); no extractor | `native_format_extractor_unavailable` |
| `dmg` artifact unpacking | same; no disk-image mount or copy | `native_format_extractor_unavailable` |
| build egress host filter | SBPL grants tcp/443 only; the declared host is validated against `NATIVE_BUILD_EGRESS_HOSTS`, but enforcement is port-level | recorded in the receipt's `build_network` |
| Windows Adapter | stays fail-closed before launch because signature/sandbox cannot be enforced | `native_adapter_unavailable` family |
| signature scope | macOS `codesign --verify --deep --strict` + `spctl --assess` only; no notarization ticket, stapling or revocation check | — |
| state collection | each slot is delivered by path into a Run-scoped writable directory; the Adapter itself captures nothing when the Run stops (that belongs to the Run's state path) | — |
| window/display containment | native_pixel window isolation is not enforced (the receipt says so in `window_containment`) | — |
| source reproducibility | `--locked` plus the Cargo.lock digest; a `.cargo/config.toml` `[patch]` or vendor override is not inspected | — |
| real toolchain download | automated tests cover the consent decision, the install plan, checksum parsing and the layout, not a live `rustup-init` download; the live path is verified on the M1 host | — |

## 4. Exclusivity rule (fail closed)

```text
bundled = ato.native.artifact is present
remote  = count of remote_url / remote_sha256 / remote_bytes   (complete at 3)
source  = count of source_url / source_commit / source_sha256 /
          cargo_lock / toolchain / build_argv / build_cache    (complete at 7)
```

- two or more of {bundled, remote, source} -> `native_artifact_source_ambiguous`
- none -> `native_artifact_source_missing`
- remote 1..2 -> `native_remote_incomplete`; source 1..6 -> `native_source_incomplete`
- `artifact` is a bundle blob only. A remote or source route carries no artifact
  blob and adds no artifact edge to the closure
  (`a_remote_native_route_validates_and_carries_no_bundle_blob`).
- `format` is `tar.gz` for a bundled artifact and a source archive; a remote
  reference may also declare `zip` or `dmg`.

## 5. M1 real-host procedure for the Lead (not run in this PR)

Preconditions: a macOS host, a terminal where Seatbelt can be applied, network
access.

1. D1 (remote): build a bundle whose D declares `ato.native.remote_*`, then
   `ato run <bundle> --derivation <D>`.
   - expect fetch -> `codesign`/`spctl` -> unpack -> launch -> HTTP observation;
   - swap the upstream asset: `native_fetch_digest_mismatch`;
   - remove the upstream asset: `native_fetch_not_found`;
   - unsigned/ad-hoc under `require-valid`: `native_signature_policy_unsatisfied`.
2. D2 (source) without approval: `native_toolchain_consent_required`, and
   nothing created under `$ATO_HOME/toolchains`.
3. Re-run with `--allow-native-source-build`: rustup-init checksum verification,
   toolchain under `$ATO_HOME/toolchains/rust/<version>-<target>`, source
   tarball digest, Cargo.lock digest, sandboxed build, launch, K observations.
4. Re-run the same D2: build cache hit (`build_cache: hit`) and no build.
5. A tarball for another commit (with a matching digest):
   `native_build_source_commit_mismatch`.
6. Teardown: no process group left behind, Run scratch removed.
7. Receipt: selected D (`derivation_ref`), `realization: native_desktop`, the
   materialization sentence in `workspace`, `build_network`, signature verdicts,
   and `fully_satisfied`. A successful build alone must not satisfy K.

## 6. Existing constraints observed but not changed by PR3

- The native profile requires one surface, one serving step and one workspace
  input; a remote or source D must meet the same shape.
- Platform admission compares the descriptor's os/arch with host facts and does
  not refuse another D because an unselected D cannot run (unchanged).

