# ato.native@1 Adapter audit (M2 / PR3)

Status: Draft (2026-11-16, PR3b). Target: the native desktop D implementation of `ato run`.
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
- D1 `zip`: a remote reference whose `ato.native.format` is `zip` is unpacked
  by `native_desktop/zip.rs` (PR3b). The central directory is the record the
  plan is built from, so every name, size, CRC32 and method is checked before
  the first byte is placed:
  - names must be relative and traversal-free with `/` separators; `..`,
    absolute paths, backslash separators, symlinks, devices, fifos, encrypted
    entries and duplicate names are refused;
  - the running uncompressed total is bounded by `MAX_EXTRACTED_BYTES`, and an
    entry whose declared size is not the size its stream produces is refused;
  - each entry's CRC32 in the central directory must match the bytes produced;
  - stored and deflate are decompressed; bzip2, zstd, lzma and ZIP64 are
    `native_format_extractor_unavailable` refusals, not a partial unpack;
  - the recorded UNIX mode is preserved with setuid/setgid/sticky stripped, and
    an entry with no recorded mode is not made executable by inference. The
    declared entry is resolved by the same `resolve_entry` the `tar.gz` path
    uses (exists, regular file, executable, inside the root).
  The unpacked tree lands in the same Run scratch directory and digests into
  the same canonical tree form as a `tar.gz`, so the receipt names what was
  actually launched.
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
  caches; outbound TCP is limited to HTTPS/443 plus DNS/53, with UDP/53 and
  the macOS resolver socket also allowed. The child environment is cleared
  and then set explicitly.
- Output cache: `sha256(recipe version, source_sha256, commit, cargo_lock,
  toolchain, target, build_argv, entry)` under
  `$ATO_HOME/cache/native-build/<hash>`. A second Run reuses it without
  building; a cached file whose bytes no longer match its manifest digest is
  discarded and rebuilt.
- Build success is not K satisfaction: the build is one realization stage, and
  the common verifier alone decides `fully_satisfied` from its observations.
  This PR does not change that path.

## 2.1 Added by PR3c: a Terminal surface observed without a probe Port

SolidCraft's D1/D2 CLI routes open no listening socket, so the HTTP-probe K
could not observe them. PR3c (M2 RFC section 8, decision A) connects the
existing `terminal_exchange` mechanism to a v2 `ato.native@1` route. No new
wire protocol and no new receipt schema version:

- authoring: `[derivation.native_surface]` with `kind = "terminal"`, `port`,
  `cols`, `rows`, and `[[derivation.native_surface.operations]]` entries that
  use the existing `terminal_exchange` keys (`input`, `output_delimiter`,
  `max_output_bytes`, `timeout_ms`, `ready_output`). There is no `probe_port`,
  and a declaration that names one is an unknown field. Every bound is decided
  by `NativeSurfaceAdapter::validate_catalog`, the same function the v1 Adapter
  and the hosted gateway call.
- logical transport: the route's one Port is `ato.pty@1` with no guest port,
  and the Application surface names that Port. An HTTP native route is
  unchanged and still requires `guest_port`.
- K: `[[contract.observation]] use = "ato.contract.terminal@1"` with `port`,
  the operation `path` and an optional `body_digest`. Exactly one observation
  per declared operation, and no HTTP/TLS/scenario observation may target a
  terminal route. This adds one Contract verifier, not a Protocol: it observes
  bytes an existing operation produced.
- Adapter: `lib/pty-host` owns the PTY (keeping
  `#![forbid(unsafe_code)]` in `apps/portable-application`); the macOS Adapter
  starts the same prepared entry under the same Seatbelt profile on that PTY.
  Ready is the declared `ready_output`, otherwise the first output. Operations
  run in authored order; a timeout, an output bound and a missing delimiter are
  that operation's measurement and the sequence stops there, so the following
  operations stay unobserved and fail closed.
- receipt: the observation keeps the existing evidence shape — `path` is the
  operation, `body_sha256` the digest of the captured bytes (delimiter
  included), `captured_bytes` their size. No schema version is added; a
  terminal run is a native run and carries `/5`'s native execution evidence.

## 3. Not implemented (recorded as incomplete)

| Item | Current state | Refusal |
| --- | --- | --- |
| `dmg` artifact unpacking | accepted in the declaration vocabulary (remote only); no disk-image mount or copy, so D1 must reference a `zip` until this exists | `native_format_extractor_unavailable` |
| v2 native pixel surface | a v2 native route authors a Terminal surface only; the pixel profile stays with the v1 native_surface Adapter | refused at authoring |
| terminal ready framing | ready is the declared `ready_output`, otherwise the first output; a program that prints nothing before its input is observable only if it declares a prompt it actually prints | timeout recorded as an unsatisfied observation |
| PTY availability | a host that cannot allocate a pseudo terminal refuses the terminal route at launch instead of falling back to a pipe | `native_terminal_start_failed` |
| `zip` entries compressed with bzip2 / zstd / lzma, and ZIP64 archives | stored and deflate are unpacked (PR3b); another method or the ZIP64 record is refused per archive/entry, never skipped | `native_format_extractor_unavailable` |
| build egress host filter | SBPL grants TCP/443 plus DNS/53 and the macOS resolver socket; the declared host is validated against `NATIVE_BUILD_EGRESS_HOSTS`, but enforcement is port-level | the in-memory realization evidence describes `build_network`; the `/5` verification receipt does not carry this field |
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
  reference may also declare `zip` or `dmg`. `zip` is unpacked (PR3b); `dmg`
  is a named, unimplemented refusal.

## 5. M1 real-host procedure for the Lead (not run in this PR)

Preconditions: a macOS host, a terminal where Seatbelt can be applied, network
access.

1. D1 (remote): build a bundle whose D declares `ato.native.remote_*`, then
   `ato run <bundle> --derivation <D>`.
   - expect fetch -> `codesign`/`spctl` -> unpack -> launch -> HTTP observation;
   - swap the upstream asset: `native_fetch_digest_mismatch`;
   - remove the upstream asset: `native_fetch_not_found`;
   - unsigned/ad-hoc under `require-valid`: `native_signature_policy_unsatisfied`.
2. D1 with `ato.native.format = "zip"` (the upstream `*-macos-universal.zip`
   asset): the same stages, with the extractor reading the central directory.
   - the real upstream zip fetches under its pinned digest and the declared
     `SolidCraft.app/Contents/MacOS/solidcraft` (or the CLI binary) is
     executable after unpacking;
   - a zip that names `../evil` or an absolute path is refused before anything
     is placed (`native_artifact_invalid`);
   - a zip whose stored bytes do not match its central-directory CRC32 is
     refused (`native_artifact_invalid`), not launched;
   - a zip that records no executable bit on the entry is refused at
     `native_entry_not_executable`, and an absent entry at
     `native_entry_missing`;
   - a `dmg` route still stops at `native_format_extractor_unavailable`.
3. D1 signature: run `codesign --verify --deep --strict` and `spctl --assess` on
   the unpacked `.app`; an ad-hoc binary under `require-valid` must fail. The
   upstream zip's signature is a property of the unpacked bundle, so this is
   the stage the Lead must observe on the real artifact.
4. D2 (source) without approval: `native_toolchain_consent_required`, and
   nothing created under `$ATO_HOME/toolchains`.
5. Re-run with `--allow-native-source-build`: rustup-init checksum verification,
   toolchain under `$ATO_HOME/toolchains/rust/<version>-<target>`, source
   tarball digest, Cargo.lock digest, sandboxed build, launch, K observations.
6. Re-run the same D2: build cache hit (`build_cache: hit`) and no build.
7. A tarball for another commit (with a matching digest):
   `native_build_source_commit_mismatch`.
8. Teardown: no process group left behind, Run scratch removed, and no
   `*.part` file beside a fetched artifact.
9. Receipt: selected D (`derivation_ref`), `realization: native_desktop`, the
   materialization sentence in `workspace`, `build_network`, signature verdicts,
   and `fully_satisfied`. A successful build alone must not satisfy K.

## 6. Existing constraints observed but not changed by PR3

- The native profile requires one surface, one serving step and one workspace
  input; a remote or source D must meet the same shape.
- Platform admission compares the descriptor's os/arch with host facts and does
  not refuse another D because an unselected D cannot run (unchanged).

## 7. Source-build DNS investigation (2026-11-17)

The handoff's unrestricted-network trial was not adopted. The existing
`with_network(false).allow_tcp_connect_remote([443])` build policy stays in
place. For remote-port grants only, the common Seatbelt renderer now permits
the two literal mDNSResponder socket paths, metadata lookup on the resolver's
parent paths (including the `/etc` and `/var` symlinks), and read access to the
literal hosts/resolver configuration files. It does not grant read access to
the whole `/private/var/run` directory or writes to those paths. A runtime
policy with only loopback Ports receives none of these resolver permissions.

On this host, unlike the earlier agent session, Seatbelt application succeeds.
Using the failed Run's actual build profile, `/usr/bin/curl` reproduced exit 6
against `https://index.crates.io/config.json`. Adding the socket rules alone
still failed: the kernel reported denied metadata reads for `/etc` and `/var`.
With those metadata grants and the resolved `resolv.conf` read, the same HTTPS
request succeeded; HTTP/80 remained denied (curl exit 7). This is a resolver
probe, not proof that the complete SolidCraft build or Contract succeeded.

The socket/configuration boundary is also visible in Chromium's
[macOS network profile](https://chromium.googlesource.com/chromium/src/+/cfbe56f17a3d7a3397a09d091252db6e3470223d/sandbox/policy/mac/network.sb).
The local kernel denials and before/after probe are the evidence for this fix.

Detected evidence gap: `NativeProvenance::build_cache` reaches the in-memory
`RealizationEvidence::workspace`, but neither it nor `build_network` is in
`VerificationNativeEvidence` schema `/5`. The original acceptance script
claims a cache hit without checking one. A successful second Run alone must
not be reported as cache-hit evidence; this needs separate evidence before
M2 acceptance can be marked complete.

