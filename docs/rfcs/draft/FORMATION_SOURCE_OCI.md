# Source → OCI materialization (6b-D2)

Status: draft implementation contract; offline integration verified, not
merged or deployed. Evidence: [first offline ledger](../../ops/formation-source-oci-d2.md),
[hardened session ledger](../../ops/formation-source-oci-d2-hardening.md).

## Boundary

Ato fixes **inputs** before a build and verifies **outputs** after it. A
Dockerfile is an untrusted build program whose semantics belong to BuildKit;
Ato neither parses nor rewrites it. `scripts/acceptance/coverage/dockerfile-
qualification-facts.py` remains a survey tool and never authorizes a build.

Inputs (`ato.source-oci-request/1`): digest-verified source archive, the
docker-default root `Dockerfile` (v0: nothing else selectable), default target,
no build args, platform, every external image as `{reference as FROM writes it,
pinned_digest, frozen archive + sha256}`, explicit `declared_transport_port`,
policy `{network: none, build_timeout_seconds, max_archive_bytes,
build: {memory_bytes, cpu_limit_millis, pids_limit, disk_bytes},
runtime: {memory_bytes, cpu_limit_millis, pids_limit}}`.

Every input is verified before a builder exists (`prepare`): request bounds,
source archive digest and tree, and each base archive's **pinned graph** —
`index.json` names exactly the pinned root; the root is the platform manifest
or an image index from which exactly one manifest for the platform is
selected; config and layers match their descriptors; every blob matches its
name; Docker's `manifest.json` names exactly that config and those layers; a
repeated tar member is refused. The root, the selected platform manifest and
the config are recorded separately.

Builder: `docker buildx --builder default` inside a **private builder
session** that the CLI starts for one job; no Docker socket is accepted from
the caller. The session owns a new work root; a fixed-size ext4 loop
filesystem (`build.disk_bytes`) holding the daemon data root, BuildKit state,
build context and saved image; a cgroup v2 subtree with `memory.max`,
`cpu.max` and `pids.max` that contains the daemon and, through
`--cgroup-parent`, every build step; a network namespace with only `lo`; an
empty daemon config (the host `daemon.json` never applies); and the socket.
Before any image is loaded the session reads back and requires: daemon
process and netns distinct from the host and caller, interfaces `lo` only, no
non-loopback route, the daemon in the session cgroup with the limits written,
the socket's listening inode held by that daemon, the reported data root and
`cgroupfs` driver. Otherwise `source_oci_isolation_unconfirmed` / `source_oci_session_refused`
and no build starts. `--network=none` only governs RUN steps; daemon-side
fetches (registries, `ADD <url>`) are stopped by the session's namespace.

Base images are copied into the session, re-verified, loaded, and the store
must then hold exactly the verified configs; they are bound with named
contexts (`<FROM text>=docker-image://ato-base/bN:frozen`); any other external
reference fails resolution. `--pull=false`, no provenance/SBOM attestation, no
entitlements (`security.insecure` and `network.host` are refused by BuildKit).

Release happens on every path after the session exists: SIGTERM to the
daemon, `cgroup.kill`, wait until the cgroup is unpopulated, unmount
everything under the work root, remove the cgroup, confirm no loop device is
backed by the session disk, delete the work root. If any step cannot be
confirmed the work root is kept with `CLEANUP-UNCONFIRMED`, the result is
`source_oci_cleanup_unconfirmed` (kept next to the original error), and no
artifact is published. Bounds while running: build output over 4 MiB aborts
(`source_oci_log_bound`); the image size is checked before `docker save`;
repacking streams under `max_archive_bytes`.

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

## Minimal design gaps in the existing OCI profile

Gaps 2 and 3 are addressed by ato#1447 / ato-api#710 (not merged): optional
`oci.working_dir`, and an Adapter that refuses image VOLUMEs no declared
state/workspace mount covers. Gap 1 is deferred (WBO has a Cmd).

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

## WBO actual: approved explicit policy

WBO root Dockerfile (unchanged pin `06e675c5…`): `FROM node:24-alpine` (tag),
`RUN apk update && apk add libcap` (unpinned Alpine packages), `RUN setcap`,
`USER 1000:1000`, `RUN npm ci --omit=dev --ignore-scripts` (lockfile +
integrity), `ENV PORT=80`, `EXPOSE 80`, `VOLUME /opt/app/server-data`,
`CMD ["/usr/local/bin/node","server/server.mjs"]`, WorkingDir `/opt/app`.

Approved scope (user, 2026-09-30): **one isolated online build of the
existing WBO pin**, then re-runs of its saved artifact. No source change, no
fallback to another image, no automatic raising of any bound. Execution starts
only after this hardening (#1446) and the profile change (#1447 / ato-api#710)
are reviewed and merged, with the exact inputs frozen first.

| Item | Condition |
|---|---|
| Phases | base acquisition and the Dockerfile build are separate phases; RUN steps never inherit the acquisition allowlist |
| Base acquisition | resolve `node:24-alpine` once; freeze linux/amd64 manifest, config, layers and archive; ≤ 200 MiB. Registry, auth and blob endpoints are fixed in that phase's allowlist; an unknown redirect target stops the phase (no wildcard) |
| Build egress | only `dl-cdn.alpinelinux.org` and `registry.npmjs.org`, through the session's proxy; no direct connection that bypasses it; no credentials |
| Limits | build 15 min; external transfer ≤ 500 MiB including base acquisition; job disk 5 GiB; archive ≤ 128 MiB; CPU, memory and PIDs fixed numerically for the build session |
| Runtime | unchanged: no outbound network, read-only root, cap-drop ALL, no-new-privileges; a start failure (uid 1000, port 80) is a typed failure, no permission is added |
| Kept | verified artifact, evidence and the authorized state only; no build cache |
| Reproducibility | apk resolution is recorded; bit reproducibility is not claimed |
| Host | check job-area headroom before the build; no host-wide prune; keep existing evidence under `~/ato-d2` |

Acceptance for WBO then reports separately: A image build, B container start,
C fresh Verifier PASS against a K frozen before execution, D a whiteboard
operation reflected to a second client, E state kept across stop and restart;
D and E fix their operations and expected values before execution, and
GET / = 200 alone is never a functional success. Anonymous volumes are
checked as well as images and containers. Weak-K receipts and the coverage-50 ledger are not rewritten.
