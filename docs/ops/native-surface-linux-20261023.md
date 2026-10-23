# Native Surface Linux producer measurement — 2026-10-23 UTC

Source base: `ato` main `17305afe0820ad7dd1e9c05c99585f837da35227`.

The new guest-agent `pixel_surface` configuration selects the common GUI producer
inside the existing supervisor. It preserves binding/rotation/process-group
lifecycle and does not open a host desktop. `netd` additionally supplies a shared
Pixel/PTY authenticated gateway lifecycle and owner-controlled sidecar.

On `ubuntu-sugamo` (Linux x86_64), TigerVNC standalone and test dependencies were
extracted as debs into `/home/ekohsuke/ato-run/.tmp/native-surface-20261023`.
System packages, services, existing containers, and host displays were unchanged.
The test ran in `bwrap --unshare-all`, a read-only private filesystem, separate
network, and temporary `/run`/`/tmp`. The Rust sidecar was compiled on that host
with Cargo 1.96.1 from a dedicated two-crate netd/ipc copy.

The [direct producer result](evidence/native-pixel-producer-linux-20261023.json)
records a complete real RFB frame, keyboard-driven pixel change, 1024×768 desktop
resize, reconnection, clipboard disabled, and closed listener after teardown.
The first frame took 618 ms in this single run; it is not an application Contract
acceptance or latency percentile. The authenticated gateway result is recorded
separately when its implementation is included.

Local checks: guest-agent library 80, binary 17, real binary smoke 1 passed.
The generic netd synthetic transport test validates authenticated first-frame
readiness and idle listener closure, separately from the real GUI measurement.
No browser shared-frame acceptance, hosted lease, Firecracker restore, GUI
application K, upload/download channel, or production deployment is claimed.
The v1 submission subset and existing portable HTTP admission are unchanged.

The first attempted Linux host, `oci-linux-test`, had a full root filesystem.
Only this task's partial 6.2 MB dependency download was removed; existing files
were preserved. Testing moved to the existing CPU host above.
