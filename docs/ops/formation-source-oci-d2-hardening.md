# Formation 6b-D2 hardening — pinned base graph, release on every path, private session

## State

**implemented · unit-tested · offline actual verified.** Host: `ubuntu-sugamo`
(x86_64, Linux 7.0.0-27-generic, Docker 29.1.3). **Not merged, not deployed. WBO
not run.** Model calls 0; no remote migration; #1421 untouched; host network
never granted. The first offline ledger ([formation-source-oci-d2](formation-source-oci-d2.md))
is kept as recorded; this ledger supersedes its builder boundary only.

- Code `6eea43ca8d7b`, `ato` sha256 `acb68668adae50bf…`. Raw data:
  [JSON](formation-source-oci-d2-hardening.json), produced by
  `scripts/acceptance/source-oci/summarize.py` from `run-all.sh` output.
- Host daemon before/after: images 68, containers 5, volumes 67 (unchanged).

## What changed (design: [FORMATION_SOURCE_OCI](../rfcs/draft/FORMATION_SOURCE_OCI.md))

1. **Base graph before load.** `index.json` must name exactly the pinned root.
   Ato then selects the one linux/amd64 manifest from the root index, checks
   its config and layers, and requires Docker's `manifest.json` to name
   exactly that graph. Every blob must match its name, and repeated members
   are refused. The root, the platform manifest and the config are recorded
   separately (fixture: root `c4634f57…` → platform manifest `9b8dad7f…` →
   config `8614a50e…`). The verified copy is what gets loaded; afterwards the
   store must hold exactly the verified configs.
2. **Release on every path.** Once a builder exists, every exit releases it.
   An unconfirmed release is returned together with the original error, and
   no artifact is published.
3. **CLI-owned private session.** The CLI accepts no Docker socket. For each
   job it creates a work root and a loop ext4 disk (`build.disk_bytes`), plus
   a cgroup v2 with memory, CPU and PID limits that also parents every build
   step. The daemon runs in its own network namespace with only `lo`, with an
   empty daemon config. Before anything is loaded, the session reads back and
   requires the isolation facts:
   - a netns separate from the host and the caller, with no routes
   - the daemon in the expected cgroup, with the limits it wrote
   - a socket inode held by the daemon
   - the expected data root and the `cgroupfs` driver

   Release uses SIGTERM, `cgroup.kill` and unmount, then confirms that no
   process, mount, loop device or cgroup remains before deleting the work
   root.
4. **Bounds while running.** Build output over 4 MiB aborts. The image size
   is checked before save, and repacking streams under the archive bound.

## Positive path

- Output `ato-source/2e483639041e@sha256:4475fdc2…`, archive `sha256:e60abf71…`
  (51189760 bytes, 6 unreferenced legacy members dropped), build 5409 ms.
- Pack: ContractRef `sha256:9bf1dde5…` (same K as the first ledger), DerivationRef
  `sha256:23c21597…` (the image digest differs per build; reproducibility is
  not claimed).
- Two air-gapped runs from empty private stores: `fully_satisfied=true` both;
  0 images / 0 containers left. **K is GET / = 200 only: typed-K PASS, not
  functional success.**

## Negatives (each with its own session; all left 0 session root / cgroup / process / mount / loop device and published nothing)

| Case | Code | Observed |
|---|---|---|
| base_graph_mismatch | `source_oci_base_graph_invalid` | Docker load config differs from the pinned graph; refused before any session exists |
| base_digest_mismatch | `source_oci_base_digest_mismatch` | archive bytes differ from the frozen digest |
| unauthorized_external_input | `source_oci_build_failed` | unfrozen `alpine@sha256:0…`: resolution fails (`network is unreachable`) |
| remote_registry_reachable | `source_oci_build_failed` | FROM a registry the host can reach: `network is unreachable`; 0 requests at the server |
| remote_input_reachable | `source_oci_build_failed` | `ADD http://<host-reachable>/x`: `network is unreachable`; 0 requests at the server |
| build_network | `source_oci_build_failed` | RUN wget: `bad address` |
| context_escape | `source_oci_build_failed` | `"/outside.txt": not found` |
| privileged_run / host_network_run | `source_oci_build_failed` | `security.insecure` / `network.host` is not allowed |
| build_timeout | `source_oci_build_timeout` | 5 s bound |
| client_disconnect | `source_oci_build_failed` | client SIGKILLed while `RUN sleep 120` ran in the session cgroup; after release, 0 processes left |
| disk_bound | `source_oci_build_failed` | 512 MiB session disk: `No space left on device` |
| log_bound | `source_oci_log_bound` | 60 steps × ~100 KB output > 4 MiB (BuildKit clips each step) |
| archive_bound | `source_oci_artifact_bounds` | reported image size > bound; not saved |
| socket_alias | CLI usage error (exit 2) | `--docker-host` no longer exists; nothing started |
| non_root | `source_oci_session_refused` | no namespace/cgroup/disk can be owned; nothing started |

Covered by unit tests only (cannot occur with a fresh session): non-empty store;
isolation mismatches (host/caller netns, extra interface, route, cgroup,
limits, socket held by another process, other data root, systemd driver);
original error + release failure kept together.

## Output ownership (fix after review of `de1d2d35`)

`materialize` now deletes `out` only when this call created it. Before the
fix, an existing `--out` directory was removed after `create_dir` failed. A
regression test covers an existing directory with a sentinel, a file, a
symlink to a directory and a dangling symlink: the contents are unchanged,
0 loads/builds happen, and the builder is released.

Actual on sugamo at `9aa2fece` (`ato` `690fbe82…`, run as root):
- existing directory, file and symlink `--out` all return
  `source_oci_output_invalid`
- the sentinels and the symlink target are intact
- 0 session roots, cgroups and mounts are left

## Findings

- `--pull=false` does not stop BuildKit from trying to resolve an unfrozen
  reference; the session namespace is what stops it (as the review expected).
- The existing portable validator refuses images with two identical layer
  digests (e.g. several RUN steps that change nothing). Unchanged here.
