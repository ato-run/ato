# Worker binary distribution (checksum and version pin)

Status: operating procedure
Date: 2026-11-03
Scope: the `ato-connected-realization-worker` binary used by user-owned
Connected Runners. This is not the `ato` CLI release channel (cargo-dist,
[getting started](../guides/getting-started.md)) and not the
`ato-formation-worker` update lifecycle
([formation worker update lifecycle](formation-worker-update-lifecycle.md)).
Contract: self-host Runner network v1, section 4.5 — the runner binary is
distributed with a version pin and a checksum plus an install procedure, and
installation is never an authority path: it does not change
`management_kind`, trust tier or enrollment.

## What this provides, and what it does not

Provided:

- `scripts/release/worker-manifest.mjs` — dependency-free Node.js generator
  and verifier for `worker-manifest.json`, the distribution manifest.
- The manifest schema `ato.worker-binary-manifest/1`: one pinned commit and
  version, plus the SHA-256 and byte size of each target binary.
- Reproduction and verification commands for a distributed artifact and for a
  source install.
- Self-tests: `node --test scripts/release/*.test.mjs`.

Not provided, deliberately:

- No binary is uploaded or published by this repository change, and no package
  manager is added. A release that publishes artifacts is a separate decision.
- No signature or attestation. The manifest carries SHA-256 checksums only, so
  it detects corruption and accidental swaps but does **not** authenticate a
  publisher. Compare the pin through a channel you trust before installing.
- No self-update. The worker never downloads or replaces its own binary; an
  operator stages and installs it.
- No hosted update endpoint or "latest" pointer. A pin is explicit.

## Manifest format

```json
{
  "schema": "ato.worker-binary-manifest/1",
  "ato_version": "0.8.1",
  "git_commit": "0123456789abcdef0123456789abcdef01234567",
  "generated_at": "2026-11-03T01:22:32Z",
  "targets": [
    {
      "triple": "aarch64-apple-darwin",
      "file": "aarch64-apple-darwin/ato-connected-realization-worker",
      "sha256": "<64 lowercase hex characters>",
      "size": 12345678
    }
  ]
}
```

- `ato_version` is the ato release version recorded with the build (for
  example `0.8.1`), **not** the worker crate's own `0.1.0`. It identifies the
  release record this binary belongs to.
- `git_commit` is the full 40-character lowercase commit the binary was built
  from. A dirty or unknown build is a release blocker; see
  [formation worker build identity](formation-worker-build-identity.md) for why
  a plausible-looking identity is worse than `unknown`.
- `targets` is sorted by `triple` and `file` is relative to the distribution
  root and must be exactly `<triple>/<binary>`.
- `sha256` covers the complete file bytes. `size` is the byte count.
- Supported triples: `aarch64-apple-darwin`, `aarch64-unknown-linux-gnu`,
  `x86_64-apple-darwin`, `x86_64-pc-windows-msvc`,
  `x86_64-unknown-linux-gnu`. An unknown triple fails closed.
- The manifest is reproducible: fixed key order, two-space JSON, one trailing
  newline, sorted targets. `generated_at` comes from `--generated-at` or
  `SOURCE_DATE_EPOCH`; without one it is the current UTC time.

## Distribution layout

```text
worker-dist/
├── worker-manifest.json
├── aarch64-apple-darwin/
│   └── ato-connected-realization-worker
├── aarch64-unknown-linux-gnu/
│   └── ato-connected-realization-worker
├── x86_64-apple-darwin/
│   └── ato-connected-realization-worker
└── x86_64-unknown-linux-gnu/
    └── ato-connected-realization-worker
```

The manifest may live inside the distribution root; the verifier skips that one
file. Every other file must be a declared target. Symlinks, nested
directories, extra files and unsupported triple directories are refused; the
tools fail closed rather than ignore an unexpected artifact.

## Generate a manifest for a local build

From the repository root, with a release build of the worker:

```sh
mkdir -p .tmp/worker-dist/x86_64-unknown-linux-gnu
cp target/release/ato-connected-realization-worker \
  .tmp/worker-dist/x86_64-unknown-linux-gnu/

node scripts/release/worker-manifest.mjs generate \
  --dist .tmp/worker-dist \
  --out .tmp/worker-dist/worker-manifest.json \
  --commit "$(git rev-parse HEAD)" \
  --version 0.8.1 \
  --require-triple x86_64-unknown-linux-gnu
```

For a byte-identical manifest across machines, pass an explicit timestamp or a
reproducible-build epoch:

```sh
SOURCE_DATE_EPOCH=1762000000 node scripts/release/worker-manifest.mjs generate \
  --dist .tmp/worker-dist \
  --out .tmp/worker-dist/worker-manifest.json \
  --commit "$(git rev-parse HEAD)" \
  --version 0.8.1
```

Confirm that an existing manifest still describes the directory (for example in
CI after a rebuild) with `--check`. It writes nothing and exits non-zero on any
drift:

```sh
node scripts/release/worker-manifest.mjs generate \
  --dist .tmp/worker-dist \
  --out .tmp/worker-dist/worker-manifest.json \
  --commit "$(git rev-parse HEAD)" \
  --version 0.8.1 \
  --check
```

`--check` compares against the existing `--out` file: the commit and version
first (a different pin reports `pin_mismatch`), then the declared targets,
checksums and sizes (`manifest_drift`). With `SOURCE_DATE_EPOCH` or
`--generated-at` set, the whole document including `generated_at` must match.
Without one, `generated_at` is not compared.

## Verify a downloaded distribution (no network)

Do this before installing. `verify` reads local files only; it does not fetch,
download or call any service. Supply the commit and version from the release
record you trust, so a manifest that was replaced together with the binary does
not pass unnoticed:

```sh
node scripts/release/worker-manifest.mjs verify \
  --dist worker-dist \
  --manifest worker-dist/worker-manifest.json \
  --commit <40-hex-from-the-release-record> \
  --version 0.8.1 \
  --require-triple x86_64-unknown-linux-gnu
```

Exit codes: `0` verified, `1` verification failure, `2` usage error.
Representative failure codes:

| Code | Meaning |
|---|---|
| `unpinned_commit` / `unpinned_version` | the manifest (or input) has no 40-hex commit or a malformed version |
| `manifest_missing_field` / `manifest_unknown_field` | the manifest shape is not the contracted one |
| `unsupported_triple` | a target triple this repository does not build |
| `unknown_file` / `unsafe_entry` | a stray file, nested directory or symlink in the distribution |
| `missing_target` | a declared target file is absent |
| `undeclared_target` | a target directory exists that the manifest does not declare |
| `size_mismatch` / `sha256_mismatch` | the binary bytes differ from the manifest |
| `pin_mismatch` | the manifest pin differs from the expected pin |
| `file_path_mismatch` | the manifest points at a different binary name |

If Node.js is not available on the target host, verify the bytes with the
platform checksum tool and the SHA-256 value you obtained with the pin from the
release record. This path needs no network either:

```sh
cd worker-dist
printf '%s  %s\n' '<sha256-from-the-release-record>' \
  'x86_64-unknown-linux-gnu/ato-connected-realization-worker' > worker-manifest.sha256

sha256sum -c worker-manifest.sha256        # GNU coreutils
# macOS: shasum -a 256 -c worker-manifest.sha256
rm worker-manifest.sha256
```

A SHA-256 comparison tells you the bytes match the value you were given. It does
not tell you who produced them.

## Install a distributed binary

Stop the service and confirm no workload is left before replacing a running
binary; the drain, stop and ownership checks are in
[Runner rollout and rollback](runner-rollout-and-rollback.md). Then stage and
install the verified file:

```sh
install -m 0755 worker-dist/x86_64-unknown-linux-gnu/ato-connected-realization-worker \
  "$HOME/.local/bin/ato-connected-realization-worker"
sha256sum "$HOME/.local/bin/ato-connected-realization-worker"
```

Use `/usr/local/bin` with the same `install -m 0755` when the service runs as
a system user. Keep the existing service flags and drop-ins; a binary swap does
not change enrollment or the credential file. Version the installed file
(for example `ato-connected-realization-worker-<commit>`) when the host keeps
more than one candidate, and point the unit at the verified path.

## Reproducible install from source (pin instead of artifact)

When no artifact is published, or when the artifact's provenance is not yet
reviewed, install from the pinned commit instead. The existing Runner guide
already documents this command; the pin is the part that makes it reproducible:

```sh
cargo install --locked --git https://github.com/ato-run/ato.git \
  --rev <40-hex-from-the-release-record> \
  ato-connected-realization-worker
```

The commit must be the same 40-hex value as the manifest's `git_commit`.
`--locked` uses the repository's `Cargo.lock`; without it the dependency graph
is resolved from the network and the install is no longer reproducible.
A source build is pinned by commit and version, not by the distribution
checksum: the published artifact's SHA-256 covers those exact artifact bytes,
and two toolchains can produce different bytes from the same commit.

## Confirm the running binary

The Runner worker has a read-only `doctor` command and no `--version`
identity line. Record what is actually installed and compare it with the pin:

```sh
sha256sum "$(command -v ato-connected-realization-worker)"
ato-connected-realization-worker doctor --work-root "$HOME/.local/share/ato-runner"
```

Write the manifest `git_commit`, `ato_version`, target triple, SHA-256 and the
`doctor` summary into the deployment record. `doctor` creates no work root,
uses no credential and launches nothing; it reports measured capabilities and
names why each unadvertised capability is missing. A missing directory is
unknown disk capacity, not free space, and diagnosis on macOS does not imply
Linux GPU/OCI compatibility.

## Rollback and a withdrawn pin

Rollback is reinstalling the previous reviewed pin, not deleting data:

1. Choose the previous record: its 40-hex commit, version and manifest SHA-256.
2. Stop new assignment and stop running workloads with the procedure in
   [Runner rollout and rollback](runner-rollout-and-rollback.md).
3. Install the previous pin with the same command — the previous artifact when
   it is still published, otherwise
   `cargo install --locked --git https://github.com/ato-run/ato.git --rev <previous-40-hex> ato-connected-realization-worker`.
4. Verify the installed file against the previous manifest (checksum for an
   artifact; commit and version for a source install), restart the unit and run
   `doctor`.
5. Record the rollback and its reason. Do not reuse a withdrawn pin until the
   reason for withdrawal is resolved.

Existing Runner credentials, work roots and state volumes are untouched by a
binary swap. A rollback must not delete or move
`<ATO_RUNNER_STATE_VOLUME_ROOT>/<runner_id>/volumes/`; state stays where it is.

## Verifying this procedure in the repository

```sh
node --test scripts/release/*.test.mjs
```

The tests generate fixtures under `.tmp/` and cover: a valid pinned manifest,
byte reproducibility, `--check` drift, tampered bytes, size mismatch, unknown
files, undeclared targets, missing targets and required triples, unpinned
commit/version, unknown fields, unsorted targets, path traversal, a different
binary name, and a malformed or absent manifest. They are Node.js unit tests of
the tool, not an acceptance of any published artifact.
