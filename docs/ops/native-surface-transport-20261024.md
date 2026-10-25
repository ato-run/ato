# Native transport acceptance — 2026-10-24

This test connects the real guest Agent PTY (PR #1512) and real TigerVNC to
the common `ato-native-surface-gateway` (PR #1507), with the execution-scoped
per-Runner assertion boundary from #1511. It does not issue a Contract receipt.

## Isolation and commands

The CPU-only Linux host ran each case inside `bwrap --unshare-all
--die-with-parent --new-session`: private PID, mount and network namespaces,
read-only system binaries/libraries, private `/proc`, `/dev`, `/tmp`, `/run`,
and only a fresh owned workspace `.tmp` directory writable as `/work`.
Neither the host home, desktop, display socket nor service credentials were
mounted. No host package or service was modified.

Inside that sandbox:

```sh
python3 /scripts/linux-terminal.py --agent /opt/guest-agent \
  --gateway /opt/gateway --work-root /work/acceptance
python3 /scripts/linux-pixel.py --producer /opt/extra/pixel-producer.sh \
  --gateway /opt/gateway --execution-scope --work-root /work/acceptance
```

The GUI dependencies were previously extracted into the task's private root,
including `xkbcomp`, X keyboard data and libraries. Initial retries failed due
to the sandbox's absent compiled-in `/usr/bin/xkbcomp` path; mounting that
private binary at the required path resolved the fixture. Production code and
isolation rules were not weakened. Each retry used a new empty work directory.

## Observed results

| Observation | Terminal | GUI |
|---|---|---|
| Surface ready | 84ms | 684ms |
| Real input | shell-produced `EXECUTED:hello` (PTY echo disabled) | changed full framebuffer bytes |
| Resize | actual `stty size`: 40 rows, 120 columns | actual 1024×768 frame, 3,145,728 bytes |
| Reconnect | retained size and subsequent real shell output | retained dimensions and frame |
| Invalid auth | expired/session/Run/lease/Runner/unscoped rejected; replay rejected | expired/session/Run/lease/Runner/unscoped rejected |
| Cleanup | gateway owner EOF, idle timeout, Agent EOF, socket/workload gone | gateway and private RFB listeners gone |

Both use `ato.runner-surface@1` and an exact Run/lease/Runner scope. The
Terminal test also confirms the readiness-only connection does not consume
the initial prompt. Keys are generated in memory and never written to the
evidence or process command line. The shared Python WebSocket helper retains
the legacy RFB default; the optional Terminal subprotocol exposes bounded
text control and binary frames through the same helper.

These are individual measurements, not performance statistics. JSON evidence
is under `evidence/native-surface-transport-20261024/`.

## Remaining acceptance boundaries

This is isolated transport acceptance, not Hosted staging, browser-frame,
persistence, application K or full Formation acceptance. Surface readiness
remains `contract_verified=false`. Product pixel profile v1 still has a fixed
viewport; private RFB resize exercised here does not enable a new product
capability. Upload, download and clipboard remain disabled. No production
deployment, GPU instance or operator key was used.
