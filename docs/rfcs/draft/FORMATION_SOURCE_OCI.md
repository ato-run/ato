# Source → OCI materialization (6b-D2)

Status: draft implementation contract; offline integration verified, not
merged or deployed. Evidence: [ledger](../../ops/formation-source-oci-d2.md).

## Boundary

Ato fixes **inputs** before a build and verifies **outputs** after it. A
Dockerfile is an untrusted build program whose semantics belong to BuildKit;
Ato neither parses nor rewrites it. `scripts/acceptance/coverage/dockerfile-
qualification-facts.py` remains a survey tool and never authorizes a build.

Inputs (`ato.source-oci-request/1`): digest-verified source archive, the
docker-default root `Dockerfile` (v0: nothing else selectable), default target,
no build args, platform, every external image as `{reference as FROM writes it,
pinned_digest, frozen archive + sha256}`, explicit `declared_transport_port`,
policy `{network: none, build_timeout_seconds, max_archive_bytes, memory,
cpu, pids}`.

Builder: `docker buildx --builder default` against a **caller-provided private
daemon** (`unix:///abs/path`; `/var/run/docker.sock` and `/run/docker.sock`
refused; store must be empty; cleared and re-checked afterwards). Base images
are loaded from frozen archives and bound with named contexts
(`<FROM text>=docker-image://ato-base/bN:frozen`); any other external
reference fails resolution offline. `--network=none`, `--pull=false`, no
provenance/SBOM attestation, no entitlements (`security.insecure` and
`network.host` are refused by BuildKit).

Outputs (`ato.source-oci-materialization/1`): `docker save` by image ID,
repacked to `oci-layout`, `index.json`, `manifest.json` and the manifest,
config and layer blobs (bytes unchanged, digest unchanged), then checked by the
existing `verify_oci_archive`. Manifest/config/layer/archive digests are
recorded as observed. Cmd, Entrypoint, ExposedPorts, WorkingDir, Volumes and
User are read from the verified config, never from Dockerfile text. The single
ExposedPort must equal the explicit binding (`EXPOSE` is a declared transport
port; HTTP suitability is the Verifier's).

Authored D: one `ato.capsule/2` OCI route with the image `ato-source/<closure12>@<manifest>`,
the platform, resource limits and `oci.workspace_mount = /.ato-workspace`
(so the capsule-only workspace cannot shadow image paths). K is GET / = 200 on
`app.http`. The route then uses the existing pack/export/run/Verifier path.

## Minimal design gaps in the existing OCI profile (not implemented)

1. **argv is mandatory.** The profile requires a non-empty `argv`, so the
   route copies the image's `Cmd` verbatim (OCI semantics; the image
   ENTRYPOINT still applies). An image with ENTRYPOINT but no Cmd is refused
   (`source_oci_cmd_absent`). Minimal change: allow an OCI route to omit argv
   and run the image default.
2. **WorkingDir is overridden.** Local and hosted OCI runs force
   `--workdir /app`. A source image whose Cmd is relative to its WorkingDir
   (WBO: `node server/server.mjs` in `/opt/app`) would not start. Minimal
   change: an optional `oci.working_dir` runtime key (absolute, validated)
   honored by the CLI and the Runner, set from the image config.
3. **Read-only root filesystem.** Runs use `--read-only`; image `VOLUME`s
   (WBO `/opt/app/server-data`) need an explicit `[[state]]` slot mounted at
   that path before writes or persistence can work.

## WBO actual: explicit policy to approve before running

WBO root Dockerfile (unchanged pin `06e675c5…`): `FROM node:24-alpine` (tag),
`RUN apk update && apk add libcap` (unpinned Alpine packages), `RUN setcap`,
`USER 1000:1000`, `RUN npm ci --omit=dev --ignore-scripts` (lockfile +
integrity), `ENV PORT=80`, `EXPOSE 80`, `VOLUME /opt/app/server-data`,
`CMD ["/usr/local/bin/node","server/server.mjs"]`, WorkingDir `/opt/app`.

Proposed opt-in request, separate from the network-denied baseline:

| Item | Proposal |
|---|---|
| Base acquisition | resolve `node:24-alpine` on docker.io once, linux/amd64 only, freeze archive + root digest before the build; ≤ 200 MiB |
| Build-time egress | allowlist only `dl-cdn.alpinelinux.org` (apk) and `registry.npmjs.org` (npm) through an egress proxy in the builder netns; no other host, no credentials |
| Limits | build ≤ 15 min, transfer ≤ 500 MiB, private data root ≤ 5 GiB, archive ≤ 128 MiB |
| Cache | none persisted across builds |
| Recorded non-reproducibility | apk package versions are not pinned by the source |
| Runtime | unchanged: `--internal` network, read-only root, cap-drop ALL, no-new-privileges; port 80 as uid 1000 is decided at runtime |
| Prerequisites | gaps 2 and 3 above (WorkingDir, state slot for `/opt/app/server-data`) |

Acceptance for WBO then reports separately: A image build, B container start,
C fresh Verifier PASS against a K frozen before execution, D a predefined
whiteboard operation observed by a second client, E the state path kept across
a restart. Weak-K receipts and the coverage-50 ledger are not rewritten.
