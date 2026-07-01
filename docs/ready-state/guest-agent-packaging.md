# Guest-agent rootfs packaging (Phase 8a-HW PR A — #912)

How the Ready-State **guest-agent** (`crates/guest-agent`) is included in a Firecracker
guest rootfs so it can receive `BindingLease` values over vsock after restore. **PR A is
packaging only** — no host↔guest vsock delivery yet (that is PR B), no `ato run` behavior
change, and the default no-binding restore is unchanged.

## What it is

The guest-agent is a small **static musl** binary that runs **inside** the guest. It
reads `HostToAgent` control messages, materializes each binding to
`/run/ato/bindings/<name>` (tmpfs, `0600`), and reports **bound-ready**. Static linking
means it runs in a minimal rootfs with no libc dependency.

## Install path + launcher contract

- **Path:** `/usr/local/bin/ato-guest-agent` (stable).
- **Runs as a foreground process** in the guest.
- **Transport mode** (`ATO_GUEST_AGENT_MODE`): `stdio` (default) frames control messages
  as newline-delimited JSON over stdin/stdout — the same framing used by the in-process
  agent smoke; `vsock` is the production guest transport (identical JSON framing over an
  AF_VSOCK connection), **wired in PR B**. Until then `vsock` exits non-zero rather than
  silently falling back.
- **Args:** the required binding names (so the agent knows what gates bound-ready).
- **Test override:** `ATO_BINDINGS_ROOT` points the tmpfs root at a temp dir.

The guest init (for a binding-required capsule) launches the agent *before* the workload
serves, so bindings are present in tmpfs by the time the app reads them. Wiring the init
launch + vsock mode is PR B/PR C.

## Building + packaging

```sh
# static binary + install into an existing ext4 rootfs at /usr/local/bin/ato-guest-agent
scripts/ready-state/package-guest-agent.sh <rootfs.ext4>
```

The script cross-builds `guest-agent` for `x86_64-unknown-linux-musl` (override with
`ATO_GUEST_TARGET`), loop-mounts the rootfs, installs the binary `0755`, and unmounts.
Requires the rustup musl target, `sudo` (loopback mount), and `e2fsprogs`.

## Verifying

After packaging, confirm the binary is present + executable in the image:

```sh
debugfs -R "stat /usr/local/bin/ato-guest-agent" <rootfs.ext4>   # mode 0100755, size > 0
# or, mounted:
test -x <mnt>/usr/local/bin/ato-guest-agent
```

The `guest-agent` crate is excluded from `cargo-dist` (`[package.metadata.dist] dist =
false`) — it is a guest-internal binary, not a distributed host CLI.

See [`binding-lease.md`](./binding-lease.md) (contract) and the Phase 8a-HW issue #912
(PR A packaging → PR B vsock `AgentChannel` → PR C live KVM E2E).
