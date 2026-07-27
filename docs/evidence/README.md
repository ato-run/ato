# Hardware evidence

Raw logs from KVM-host runs that gated a merge. Kept in the repo so the claim
survives the machine: a builder's scratch directory is not a durable record, and
the number in a PR comment is only as good as the log behind it.

Each entry states how to regenerate it, because an unreproducible measurement is
an anecdote.

---

## `ready-state-acceptance-e2e-2026-07-27.txt`

The gate for **ato#1155** — the first end-to-end completion of the
interactive-capture acceptance loop on real hardware. Before that fix the loop
could not complete at all: the disposable restore took the slot lock its own
hold was still holding.

```text
host          Hetzner CX53, root@65.109.37.38
kernel        6.8.0-124-generic
Firecracker   v1.16.0
e2fsprogs     mke2fs 1.47.0 (5-Feb-2023)
date          2026-07-27
result        10 / 10 pass
live services ato-runner-agent + ato-snapshot-builder active THROUGHOUT, untouched
```

| stage | min | median | max |
|---|---|---|---|
| hold_ready | 4616 ms | 4664 ms | 5486 ms |
| capture | 5319 ms | 5662 ms | 6662 ms |
| release | 458 ms | 468 ms | 491 ms |
| verify | 3211 ms | 3258 ms | 3298 ms |
| total | 17371 ms | 17748 ms | 19110 ms |

Ten runs is not a p99 sample and is not reported as one.

### What the log proves, and how

Attribution of the restored guest cannot come from a nonce baked into the image:
restore resumes identical memory, so the held guest and the restored guest answer
one identically. The evidence is a combination, and every run carries all of it:

1. before release the held guest answers — the address is live;
2. after release `held_pid_gone`, `lock_released`, and
   `pre_restore_connect_failed` over **five consecutive probes** — nothing is
   serving there;
3. after restore the address answers and echoes a fresh 128-bit nonce minted
   *after* the candidate was sealed.

The echo is driven by `[seal_at]` itself, so `acceptance=accepted` **is** the
nonce proof. A readiness 200 alone would not be.

`verify_ms` is the load-bearing number. A run that rejects before restoring
records ~119 ms; these record ~3.3 s, which is a real restore plus `seal_at`.

### Budget verdict recorded here

Production allowance on that host was 60 s (`acceptance_config_for_seal_at`) +
240 s (`ATO_FC_BOOT_TIMEOUT_S`) + 120 s slack = **420 s**, against 3.298 s
measured worst case — roughly 127× headroom, spread ±1.4%. Kept as-is.

Caveat stated plainly: the E2E supplies its own `AcceptanceConfig`
(`total_deadline` 600 s), so what was measured is the **work**, not the
production budget path itself.

### Contention

Classified **A (no conflict)**. The runner runs netns slots
(`ato-slot-{0,1}.lock`) while the builder is root-ns (`fctap0.lock`) — different
lock files by construction — and the E2E isolated itself again under
`/tmp/atoe2e-<run>/`. No interference observed; `firecracker` process count 0
before and after.

Also stated plainly: the production builder held **no live job** during the
window, so contention was not exercised under load.

### Regenerating it

```sh
# 1. fixture guest image, through the EXISTING v1 recipe producer
cd tests/fixtures/ready-state-acceptance
#    rewrite [seal_at] with the run-unique guest IP first (no {addr} templating
#    exists yet — ato#1158)
ato build          # prints: Guest image: <path>.img

# 2. test binary — KVM hosts typically have firecracker but no cargo
docker run --rm -v "$PWD":/src -w /src rust:1.96 \
  cargo test -p snapshot-builder --bin snapshot-builder --no-run

# 3. the runs
ATO_FC_BIN=/usr/local/bin/firecracker \
ATO_FC_KERNEL=/var/lib/ato/kernel/vmlinux-5.10.223 \
ATO_FC_TEST_ROOTFS=<guest image from step 1> \
ATO_E2E_TEST_BIN=target/debug/deps/snapshot_builder-<hash> \
scripts/e2e/ready-state-acceptance-e2e.sh 10
```

**Never run `scripts/ready-state/run-uffd-kvm-smokes.sh` on a live runner.** It
begins each test with `pkill -9 firecracker` and `ip link del fctap0`, which
kills production VMs and deletes the production builder's tap. The harness above
isolates tap, work root, IPs and scratch under an `atoe2e` run prefix and
**aborts** rather than reusing or deleting anything it did not create.

### Product findings this run produced

- `release()` does **not** unlink the vsock UDS — the file remains with no
  listener (`file_remains=true` in the log). → ato#1157
- A non-`source_lost` capture failure has no backoff and no failure budget: 356
  full snapshots in ~15 minutes on an earlier run. → ato#1160
