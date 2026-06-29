# Firecracker M0 Spike — throwaway feasibility gate

> **Throwaway.** This directory is the M0 feasibility spike from
> `ato-plans/ready-state-capsule-implementation-plan.md` §6. It is **not** product code and
> is deleted once the go/no-go is recorded. Nothing here is wired into the ato build.
>
> **Everything except the actual VM boot is prepared so M0 runs the moment a `/dev/kvm`
> host exists.** The boot/snapshot/restore steps are the only KVM-gated parts.

## Why this exists

Before touching ato core, prove host-controlled snapshot/restore **works, is fast, and leaks
no state**. Output is a single receipt + a go/no-go. This is the only thing that can sink the
Ready-State line.

## Host prep (step 0 — gating)

```sh
# 1. KVM must be present and usable. On OCI A1 *VM* shapes this FAILS (no nested virt).
#    Use an OCI bare-metal shape (BM.Standard.A1.160, aarch64, has /dev/kvm) or an
#    x86_64 box with VT-x + nested virt.
ls -l /dev/kvm            # must exist and be rw for the runner user
[ -r /dev/kvm ] && [ -w /dev/kvm ] && echo "KVM OK" || echo "KVM MISSING — pick another host"

# 2. The chosen host's arch FIXES the MVP arch. Record it.
uname -m

# 3. Pin Firecracker + jailer to a known version (snapshots are VMM-version sensitive).
#    See ./versions.env. Download from the Firecracker releases for the host arch.
. ./versions.env && echo "pinned firecracker=${FC_VERSION} arch=${FC_ARCH}"
```

## Artifacts in this directory

| File | Purpose | KVM needed? |
|---|---|---|
| `versions.env` | pinned Firecracker version, guest kernel, rootfs, CPU template | no |
| `build-guest.sh` | build guest kernel (vmlinux) + minimal rootfs (Alpine + tiny init) + bake the hello app | no (builds artifacts) |
| `run-spike.sh` | boot → healthcheck → pause → CreateSnapshot → LoadSnapshot → resume → re-expose → measure | **yes** |
| `leak-test.sh` | write a unique marker post-restore; prove a fresh restore does NOT see it (20 cycles) | **yes** |
| `mismatch-test.sh` | attempt restore under a different CPU template / FC version → must fail | **yes** |
| `no-secret-test.sh` | inject a fake secret at runtime only; grep snapshot mem/vmstate → must be absent | **yes** |
| `vsock-guest-agent-protocol.md` | the host↔guest sanitizer RPC contract (interface only) | no |
| `GO-NO-GO.md` | acceptance criteria + the receipt template to fill in | no |

## Run order (on a KVM host)

```sh
. ./versions.env
./build-guest.sh                 # KVM-free: produces vmlinux + rootfs.ext4 + hello app
./run-spike.sh                   # boot→snapshot→restore→measure (writes receipt.json)
./leak-test.sh 20                # 20-cycle cross-restore state-leak check
./mismatch-test.sh               # runner_class mismatch must be detected
./no-secret-test.sh              # seal-before-bind: secret never in snapshot
# then fill GO-NO-GO.md from receipt.json + the test outputs
```

## What this proves (maps to runner_class + seal invariants)

- restore→ready latency (File-backed memory) → motivates CapsuleFS hotset/UFFD
- **zero cross-restore state leak** → the permanent regression test for `ato run`
- runner_class mismatch reliably detected → validates the fail-closed gate (capsule `runner_class.rs`)
- secret never present in snapshot → validates seal-before-bind + the no-secret scanner (snapshot `scanner.rs`)
