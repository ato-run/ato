# Firecracker supervisor-mode spike (v1.2)

Hardware proof, on real Firecracker, of the **guest-agent supervisor mode** the
v1.2 `delivery = "env"` secret binding needs (`extensions/providers/snapshot/guest-agent/src/supervisor.rs`).
This validates the mechanism before the Rust `rootfs_builder` / `FirecrackerBackend`
wiring (the follow-up) — the same "bash spike before the backend" pattern the M0
Firecracker spike used.

## What it proves

`supervisor-e2e.py` drives the full build→seal→restore flow against a real
microVM whose init IS the guest-agent (vsock supervisor):

- **build**: boot the supervisor rootfs → deliver a **placeholder** binding over
  vsock → the agent starts `app.py` with it (env composed **in the workload
  child**, never the agent) → `/health` up → `StopWorkload` (kill app) → `Revoke`
  (scrub tmpfs) → snapshot mem + vmstate.
- **restore**: load + resume → deliver the **real** key over vsock → the agent
  **restarts** `app.py` with the real env → `/health` up → `/keyhash` == the real
  key's hash (restart-with-env), while the real key was never delivered at build.

## Receipt (`RECEIPT.json`, GCP `ato-kvm-p3b`, FC v1.16.0, kernel vmlinux-5.10.223)

| check | result | meaning |
|-------|--------|---------|
| `placeholder_health` | ✅ | supervisor started the workload from a bound placeholder |
| `health_down_after_stop` | ✅ | `StopWorkload` + `Revoke` idled the app + scrubbed tmpfs |
| `real_key_absent_from_seal` | ✅ | **security invariant**: the real credential is never in the seal |
| `restart_with_real_env` | ✅ | **D1 mechanism**: restore restarts the app with the real env, correct key live |
| `placeholder_absent_hardening` | ⚠️ false | kernel-gated defense-in-depth (see below) |

**Verdict: PASS** on the security-critical invariants.

## The two findings this spike surfaced

1. **The agent must never hold the value in its own heap.** The first run left
   the placeholder resident because the agent read the binding into its own
   address space to compose the env; a long-lived init's heap is never freed, so
   `init_on_free` can't zero it. Fixed in `supervisor.rs`: the agent builds a
   `SpawnPlan` of tmpfs **paths**, and the workload child reads the value at
   `exec` (`sh -c 'export VAR="$(cat PATH)"; exec CMD'`). The value now lives only
   in the child's environment.

2. **`placeholder_absent_hardening` needs a guest kernel with `init_on_free`.**
   After the app is killed, its env pages free but are only zeroed if the guest
   kernel was built with `CONFIG_INIT_ON_FREE_DEFAULT_ON` / page poisoning. The
   stock firecracker-ci kernel lacks it, so the **non-secret** placeholder lingers
   in freed pages. This is **not a leak** — the placeholder is a marker, and the
   security invariant (no *real* secret in the seal) holds structurally because
   the real secret is delivered only at restore into a VM that is never
   re-snapshotted (post-bind-dirty). A production builder kernel with
   `init_on_free=1` makes this check pass; the Rust build flow (follow-up) should
   set that boot arg for supervisor builds and record the kernel capability.

## Run

```sh
# on a KVM host with docker + firecracker + a guest kernel:
bash build-supervisor-rootfs.sh <agent-bin> app.py supervisor.ext4 512
sudo env ATO_FC_BIN=… ATO_FC_KERNEL=… ATO_FC_ROOTFS=supervisor.ext4 \
     ATO_FC_WORK=/tmp/sup-e2e python3 supervisor-e2e.py
```
