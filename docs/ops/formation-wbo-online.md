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
