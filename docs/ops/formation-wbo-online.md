# Formation — WBO isolated online build (6b-D2 follow-up)

Expectations were fixed before any execution in
[formation-wbo-expectations.json](formation-wbo-expectations.json).
There was no deploy, no remote migration and no model call; #1421 is untouched.

## Implementation (branch `feat/formation-source-oci-online`)

- **netd egress** (`045e1f70`): a port allowlist, one transfer budget shared by
  every tunnel (crossing it cuts the tunnels and refuses new CONNECTs), and
  an explicit bind address. Existing policies are unchanged.
- **Egress gate** (`source_oci::egress`): netd's CONNECT proxy with
  - exact host and port allowlists
  - denied private and special ranges, so DNS rebinding cannot reach the host
    or the LAN
  - the budget, with every decision kept as evidence
- **Base acquisition** (`source_oci::acquire`, `ato __source-oci-acquire-base`):
  a phase separate from the build. One docker.io `name:tag` is resolved
  through a gate listing only the registry, auth and blob endpoints; redirects
  may go only to listed hosts over HTTPS. Every digest is verified, and the
  written archive must pass `verify_base_archive`.
- **Egress session** (`session.rs`):
  - The namespace gets a private bridge whose only way out is a veth to the
    gate.
  - Host side: INPUT accepts only the gate port from that veth and FORWARD
    drops it.
  - Namespace side: DOCKER-USER confines build steps to the gate, OUTPUT drops
    everything else, IPv6 is off and there is no default route.
  - The wiring is read back before any build.
  - Release stops the gate, removes the tagged host rules and the veth, and
    confirms that none remain.
- **Preflight**, run from a container of the frozen base on the build network
  before the build. The build starts only if all of these hold:
  - direct dials are blocked: a listed host's own address, `1.1.1.1:443` and
    the host's SSH port
  - an unlisted CONNECT returns 403
  - a listed CONNECT returns 200
- **Build**: RUN steps get the proxy through BuildKit's predefined proxy build
  args; the Dockerfile is not changed.
  - A budget crossing is `source_oci_egress_bound`.
  - A failed build's error carries the gate record.
  - An `apk info -v` inventory of the built image is recorded (offline).
- **Authoring**: image VOLUMEs must equal the explicitly `authorized_state`
  mounts (`[[state]]`), and a non-`/app` WorkingDir becomes `oci.working_dir`.

Tests:
- portable lib: source_oci 21, egress 1, acquire 1 and isolation 4 all pass
- netd egress: 26 pass
- clippy is clean on Linux and macOS

## Egress checks that exchange no application data (ubuntu-sugamo, `online-negatives.sh`)

| Case | Result |
|---|---|
| bypass_blocked | preflight blocked all 3 direct dials (`199.232.150.132:443`, `1.1.1.1:443`, gate host `:22`), unlisted CONNECT 403, listed CONNECT 200; the RUN step's bypass attempts (no proxy variables; a raw dial to the CDN address) all failed, so the build succeeded |
| unlisted_host | `wget https://example.com` inside RUN failed (gate 403): `source_oci_build_failed` |
| transfer_budget | 1 KiB budget crossed by bytes relayed toward a listed host: `source_oci_egress_bound` |

In all three cases: 0 session roots, cgroups, tagged host rules and veths
were left, and host iptables (140 rules) and links (9) were unchanged.

## Run 1: stopped in the base-acquisition phase (as designed)

- Code `88ec574c`. The binary was built from `eff9c63e`; the later commits
  change only error text, scripts and docs.
- The WBO archive matched the pin (`sha256:8af51caa…`). Disk had 28.4 GB free
  before the run.
- The tag manifest and the amd64 manifest were fetched from
  `registry-1.docker.io` with an anonymous token from `auth.docker.io`.
- The first blob GET was redirected (HTTP 307) to
  **`production.cloudfront.docker.com`**, which is not the blob endpoint that
  had been fixed in the allowlist (`production.cloudflare.docker.com`, a wrong
  assumption). The redirect policy refused it and the phase stopped with
  `source_oci_base_acquisition_failed`.
- The redirect target was confirmed afterwards with a headers-only request
  (token from `auth.docker.io`, HEAD-style GET on `registry-1.docker.io`; no
  blob body was downloaded).
- No build ran, and no runtime or functional check ran (A–E not reached). Host
  daemon, iptables and links were unchanged; no session was created.

The allowlist was not widened. Proceeding needs an explicit decision to
replace the blob endpoint with the observed exact host
`production.cloudfront.docker.com` (no wildcard). Any further unlisted
redirect would stop the phase again.

## Findings

- `ato app stop` waits 5 s for the worker's acknowledgement. An OCI stop took
  longer (the Run ended after about 45 s in a dry run), so the command reports
  a timeout while the worker's cleanup completes; the driver waits for the
  Instance to have no active Run. This is pre-existing and unchanged here.
- After a durable restart, the Run's receipt names a different ContractRef and
  one more observation: the continuation is re-sealed with its state. This is
  existing semantics, recorded as observed.

## Run 2 (after the user-approved blob endpoint `production.cloudfront.docker.com`)

- Code `4553d368`, `ato` sha256 `dfc737fe…`. The WBO archive matched the pin;
  28.3 GB of disk was free.
- **Base acquisition: done.**
  - `docker.io/library/node:24-alpine` resolved to root `sha256:ebfe2f90…`;
    linux/amd64 manifest `sha256:83f1c388…`; config `sha256:c1088495…`;
    4 layers.
  - Content 61,648,926 bytes; archive `sha256:c60f7d0d…` (61,652,992 bytes).
  - The gate moved 61,798,832 bytes, with CONNECTs only to
    `registry-1.docker.io`, `auth.docker.io` and
    `production.cloudfront.docker.com`, and 0 refusals.
  - Build egress budget: 524,288,000 − 61,798,832 = 462,489,168 bytes.
- **A (image build): stopped at the artifact bound.**
  - BuildKit completed the WBO Dockerfile inside the egress session (so apk
    and npm went through the gate).
  - The built image reports 372,919,274 bytes uncompressed. That is over the
    128 MiB archive bound, so it was refused before `docker save` with
    `source_oci_artifact_bounds`, and nothing was published.
  - The bound was not raised.
  - Session roots, cgroups, tagged host rules and veths left: 0.
  - This run's gate record and preflight were not published (the failure path
    dropped them). This was fixed afterwards (`350a92f3`): every failed online
    build now carries the egress record and the preflight.
- B–E were not reached.

The transport archive is a classic-store `docker save` repack whose layers are
uncompressed tars, so the bound applies to uncompressed layer bytes. Whether a
compressed-layer archive would fit within 128 MiB was not measured.

## Archive bound change (user decision after run 2)

The portable OCI transport bound and the source→OCI archive bound were raised
from 128 MiB to **512 MiB**. A bundle may carry that archive as base64, so the
bundle bound is now 768 MiB (`7d89439e`). Hosted import keeps its own limits;
the Hosted validator was not rebuilt or deployed.

## Run 3 (reusing the base frozen in run 2; no second acquisition)

Code `7d89439e`, `ato` sha256 `a7b929ac…`. Host daemon was 68/5/67 before and
after; host iptables had 140 rules and 9 links before and after.

This is the second online build of the pin: run 2's build finished but its
artifact was refused before save. Run 2's build egress was not recorded
(fixed since), so the total moved across runs 2 and 3 is at most
61.8 MB (acquisition) + run 2's unrecorded build egress + 12.3 MB.

- **A — image build: PASS.**
  - Preflight from the frozen base on the build network:
    - direct dials blocked: `199.232.150.132:443`, `1.1.1.1:443`, gate
      host `:22`
    - unlisted CONNECT: `403 Forbidden`
    - listed CONNECT (`dl-cdn.alpinelinux.org`): `200`
  - Gate:
    - allowed: `dl-cdn.alpinelinux.org:443` ×3 and `registry.npmjs.org:443`
      ×15
    - refused: `example.com` ×1 (the preflight)
    - 12,340,498 bytes moved, budget not exhausted
  - Build 23 s.
  - Image `ato-source/eec4d71131ae@sha256:95a49001…`; archive
    `sha256:5bc5faee…` (387,392,512 bytes, 11 layers, 11 unreferenced legacy
    members dropped).
  - Image config:
    - Entrypoint `docker-entrypoint.sh`
    - Cmd `/usr/local/bin/node server/server.mjs`
    - WorkingDir `/opt/app`
    - User `1000:1000`
    - VOLUME `/opt/app/server-data`
    - ExposedPorts `80/tcp`
  - Authored route: `oci.working_dir = /opt/app` and `[[state]] server_data`.
  - `apk info -v`: 23 packages recorded.
  - Session residue: 0.
- **Pack/export.** K `sha256:9bf1dde5…` (GET / = 200), D `sha256:c0c95774…`,
  offline bundle 516,538,373 bytes.
- **B — container start: FAILED (typed), stopped here.**
  - On an empty private store with no route, the container exited at once with
    **exit code 126**.
  - A diagnosis under the same flags (read-only, cap-drop ALL,
    no-new-privileges, uid 1000, `--workdir /opt/app`, state bind) shows
    `/usr/local/bin/docker-entrypoint.sh: exec: line 11: /usr/local/bin/node:
    Operation not permitted`.
  - The image's `RUN setcap CAP_NET_BIND_SERVICE=+eip` gives `node` a file
    capability (`cap_net_bind_service=eip`). The kernel refuses to execute such
    a binary when that capability cannot be granted (bounding set emptied by
    cap-drop ALL, no new privileges).
  - No capability, root or network was added.
  - The Adapter previously reported this as an address parse error; it now
    reports `OCI container has no network address (status=exited exit=126 …)`
    (`d647fcb3`, re-run on the same artifact).
- **C, D, E: not reached.** No receipt was produced; the functional checks
  were not run.
- Private store after the attempt: 0 containers, 0 volumes. Host daemon
  unchanged.

WBO therefore needs, at runtime, either a capability the profile does not
grant (`CAP_NET_BIND_SERVICE` for the file capability to apply) or a source
without the `setcap` step. Neither is decided here: the Dockerfile is not
rewritten and permissions are not relaxed automatically.
