# Formation P0 unblock 2 — acceptance (2026-09-23)

Base: ato `main` `c0de5efb` (P0 unblock 1 merged as #1383); ato-api `main`
`d4800415` as the local coordinator (`wrangler dev --local`, local D1/R2, no
remote migration). Branch `feat/formation-p0-unblock-2`.

## Changes

1. **Build process-group invariant** (`build.rs` `run_step`) — a step returns
   only after its whole process group is gone, on success, failure and
   timeout. After the step's own process exits (observed with `WNOWAIT`, so
   its pid — the group id — stays reserved) its pipes get a short grace, the
   process is reaped, and any surviving group member is stopped (TERM, then
   KILL) and proven gone before the result is returned. A step with nothing
   left behind returns at once. Failing to make the output pipes
   non-blocking, or a `poll` error other than `EINTR`, stops the group and
   fails the step. (Previously a descendant holding the pipes was left
   running after 2 s, one that closed them was never noticed, and the test
   asserted that and cleaned up with `pkill`.)
2. **Artifacts carry contained symlinks** (`pack.rs`) — a third entry kind,
   packed as a link (GNU header, target verbatim, mode 0777, uid/gid/mtime 0),
   re-validated at pack time. Absolute and escaping links are refused.
3. **One containment rule** — `ato_formation::containment::validate_contained_symlink_target`,
   used by the source resolver and by pack; the two identities stay separate.
4. **No host path in requester-visible failures** — work root, out dir, temp
   dir and home are replaced before an attempt leaves the driver; pack errors
   name tree-relative paths.

## Acceptance — searxng upstream, unmodified

Source `searxng/searxng@2ed96e6fcfc96ca1045155fc52a12f5f7b070417`, fresh
checkout, symlink `utils/templates/etc/apache2 -> httpd` present. Route
`benchmarks/formation/p0/searxng/capsule.toml` (unchanged since the baseline).

### K0 — `--mode all`, no Browser Contract

Satisfy `01M371QE0KXP3SN4PJ0KJ7M96D` → **satisfied**. K
`sha256:ae9e1150e80da888866a537ff66e4cfb9c9d4597b4f1ef0f3f3b4441bfd7569f`,
D `sha256:65eaf36b0171fdc5c562fc136e20bf889f90e974a8d263c280bdb6db5963f2a8`
(the same on both Runtimes).

| | Linux aarch64 `rt_oci-arm64` | Linux x86_64 `rt_sugamo-x86` |
|---|---|---|
| Candidate | admissible, rank 1 | admissible, rank 2 |
| Source (resolver v2) → build → realization | ok | ok |
| Typed K: `/healthz`, `/`, source identity | satisfied ×3 | satisfied ×3 |
| Attempt | `01M371QE1KHX4E0A0MEGGAZMX6`, **pass**, 11:51:58 → 11:53:17 (79 s) | `01M371SV5RAAEVW8WQ8CXF6FAR`, **pass**, 11:53:18 → 11:54:10 (52 s) |
| Artifact (materialization ref) | `sha256:ba87d1eb8a0689a65c8ed36607d15d94de9a08ad8729a54b211b154baeaa204a` (121.7 MB, 4 422 entries) | `sha256:08e7fff3f0946bf8fb8a57b9e1246d9d16f7a6f20b0b0c2ac4447b9597d91405` (122.0 MB, 4 422 entries) |
| Symlink entries in the artifact | 1: `lrwxrwxrwx 0/0 0 1970-01-01 utils/templates/etc/apache2 -> httpd` | 1: the same |
| Capability profile | `sha256:084a0a06…` | `sha256:4d756f3b…` |
| **VerifiedRoute** | **`01M371SV5EJ8YAQ5NDA8BEMW9X`**, receipts `http_contract`, agent 0.1.0 | **`01M371VETHZTKD6DPRM7NGA4Z5`**, receipts `http_contract`, agent 0.1.0 |

The macOS Runtime was filtered (`runtime.process`, `containment`,
`toolchain.root`). The two artifact digests differ because the installed
dependencies are architecture-specific builds; each file's SHA-256 matches
its materialization ref. The requester-visible result contains none of
`/home/`, `/Users/`, `/private/`, `/tmp/`, a token path, or a work root.

**Level 5 on both architectures: the same K and D, two VerifiedRoutes, distinct
Runtime and profile evidence.**

### K1 — Browser Contract, once, x86_64

Satisfy `01M371X33KF9PBTGV6GVWDNZP0`, attempt `01M371X33W1Y9XWF6ZXG5TPCKV`
(46 s): typed K satisfied ×3; Browser Contract **inconclusive** —
`agent_provider_refusal` (DeepSeek: "Content Exists Risk"), fourth time on
this page. Recorded as INCONCLUSIVE; no fallback, no retry, no prompt change,
no provider change. Provider failure, not an application failure: the typed
K passed in the same attempt. Contained verifier:
`separate-sandbox+empty-environment+chrome-no-sandbox`.

## Tests

| Suite | macOS | OCI aarch64 | sugamo x86_64 |
|---|---|---|---|
| `build` unit (10: 3 new process-group cases — a descendant holding the pipes, one detached from them and writing to a file, a failing step with a descendant — each leave no survivor and no further writes; a clean step returns in < 1 s; the drain, typed failure and timeout cases kept) | 10/10 ×3 | 10/10 ×5 | 10/10 ×5 |
| `pack_v1` (8: golden symlink-free digest recorded from the previous `pack_tree`; link packed as a link and read through after unpacking; deterministic, target is identity; absolute / climbing / descend-then-climb refused by tree path, no host path) | 8/8 | 8/8 ×5 | 8/8 ×5 |
| `ato-formation` lib (97, incl. the shared containment rule) | pass | pass | pass |
| `local_formation_v1` (16: + a source with a link is Formed and its artifact holds the link; a store failure keeps verdicts and names no scratch path) | pass (Linux cases Filtered) | 16/16 | 16/16 |
| `source_symlinks_v1`, `runtime_network_v1`, `browser_verifier_*`, `temporary_realization_v1`, `static_lane_v1`, `authoring/intent/preset` | pass | pass | pass |
| `sandbox_v1` | pass (skips) | 10/11 ¹ | 10/11 ¹ |

A mutation that skips the group stop fails all three process-group tests.

¹ `a_step_that_declared_no_network_does_not_get_one` looks for the worker
binary beside the test binary — pre-existing, unchanged here. Also
pre-existing and not touched: `local_formation_v1`'s intermittent parallel
port collision on sugamo (it passed in this run; main reproduces it).

## Cleanup

Workers on all three Runtimes stopped; coordinator, tunnels stopped; runner
tokens and sugamo's temporary key file removed; Runtime work/out directories
removed (artifacts included, after the evidence above was taken); no
searxng, browser or verifier scratch left on either Linux host; source
checkout removed. OCI's `~/.config/ato/formation-browser.env` kept for re-runs.

## Follow-ups (not in this change)

- Hosted Runner: `connected-realization-worker`'s workspace unpacker accepts
  files and directories only, so it refuses an artifact that carries a link.
- Hosted / control-plane source projection alignment (ADR-023).
- Next primitive by the P0 baseline: projection of authored `exec`
  preparation/build steps (16/20).
