# Issue #1690 — OCI Service Group on macOS + Docker Desktop

Status: Current (evidence for the change; not a specification)

Language: English. Japanese summary at the end.

This page records what was run for `ato-run/ato#1690`: an OCI Service Group
capsule could not run on macOS + Docker Desktop because admission refused every
non-Linux host (`oci_needs_native_linux`), the adapter dialled container
addresses on a network the CLI cannot reach, and the stop path could leave a
workload's children running.

It is split across five Draft PRs against `stg`; this page is the evidence
ledger, not a second source of truth.

## Pull requests

| Phase | Branch | PR | What it changes |
|---|---|---|---|
| A | `feat/1690-a-process-group-reclaim` | [#1691](https://github.com/ato-run/ato/pull/1691) | Reclaim the whole process group on stop; scope Seatbelt outbound connect to `localhost:{port}`. |
| B | `feat/1690-b-host-facts-preflight` | [#1694](https://github.com/ato-run/ato/pull/1694) | `lib/ato-host-facts`: observed host facts, reason codes, the execution decision; `ato pack` reports it, `ato run` admits on it. |
| C | `feat/1690-c-oci-relay-connector` | [#1695](https://github.com/ato-run/ato/pull/1695) | `PortConnector` (direct bridge / relay), the relay container and its protocol, connector choice from host facts, connector evidence in the receipt. |
| D | `feat/1690-d-native-service-group` | [#1693](https://github.com/ato-run/ato/pull/1693) | Native multi-service Derivation, shared supervisor, peer broker. |
| E | `feat/1690-e-native-state-path` | [#1692](https://github.com/ato-run/ato/pull/1692) | `mount_required = false` state slots delivered by path. |

## macOS + Docker Desktop acceptance (Phase C)

Host: macOS 15.8.1, arm64, Docker Desktop 29.1.3 (daemon `aarch64`,
`OperatingSystem: Docker Desktop`). Worktree `.tmp/issue1690/wt-b`,
`ATO_HOME` inside the worktree.

```sh
ato pack samples/oci-service-group-proof -o .tmp/1690/oci.capsule
ato run  .tmp/1690/oci.capsule --no-open --verification-receipt .tmp/1690/mac.json
```

`ato pack` (exit 0; the bundle is immutable and this host is not part of it):

```text
bundle_sha256=sha256:2c88d86cc1c10039a9454b57ad0962986fabb0aa951970af9567c6b4dcb0a902
contract_ref=sha256:68228e9bf2bcadc375cc70747bb23e672290a1a16774d2ed6df387488db5e529
derivation_ref=sha256:a6ff45c1fb10cb349003b0703a0b4fdf7633879a7af361b9e04997514108148c
capability derivation_ref=sha256:a6ff45c1… route=OCI Service Group platform=linux/amd64 connector=relay runnable=yes state=Advertised
capability reason=docker_bridge_unreachable_vm message=the daemon runs in a VM (daemon OS "Docker Desktop", client OS macos), …
capability warning=platform_emulated message=linux/amd64 runs through emulation on this host (…); it works and it is slower, so the receipt records the emulation rather than refusing the Run
```

`ato run` (exit 0) wrote `.tmp/1690/mac.json` (schema
`ato.contract-verification-receipt/4`):

```json
{
  "fully_satisfied": true,
  "contract_ref": "sha256:68228e9bf2bcadc375cc70747bb23e672290a1a16774d2ed6df387488db5e529",
  "derivation_ref": "sha256:a6ff45c1fb10cb349003b0703a0b4fdf7633879a7af361b9e04997514108148c",
  "execution": {
    "realization": "oci_service_group",
    "connector": "relay",
    "relay_image_digest": "sha256:27764d7cb945821b998ac29fc896da3319fe8f3afab53456931ad7be2b6919f1",
    "relay_container_id": "2ff838e01fc87109a9ab21801e1d64514c95c81fca936b71703a8bf7b278d485",
    "platform": "linux/amd64",
    "platform_emulated": true,
    "image": "docker.io/nginxinc/nginx-unprivileged@sha256:28d91bdce70ad09025ea901458fdd149259d8e05982ade79d4ef2c0d9470eb48",
    "container_id": "ecaf11c45a35965b7523bece82205e3b665a92ed06c1e05816f526db3d172967"
  },
  "observations": [
    {"id": "backend-through-surface", "outcome": "satisfied", "evidence": {"status": 200, "path": "/"}},
    {"id": "surface-ready", "outcome": "satisfied", "evidence": {"status": 200, "path": "/healthz",
      "body_sha256": "sha256:dc51b8c96c2d745df3bd5590d990230a482fd247123599548e0632fdbf97fc22"}}
  ]
}
```

Two services, both container ids and images recorded:

```text
backend efd7995d160b… docker.io/traefik/whoami@sha256:4f90b33d…
web     ecaf11c45a35… docker.io/nginxinc/nginx-unprivileged@sha256:28d91bdc…
```

### Ports: nothing internal is published

While the Run was up:

```text
$ docker port ato-sha256a6ff45c1fb10cb3490-web-37225-3
(no output)
$ docker port ato-sha256a6ff45c1fb10cb3490-backend-37225-2
(no output)
$ docker port ato-relay-51145a45da2061a5
(no output)

$ docker inspect … --format '{{json .HostConfig.PortBindings}}'
{}                      # every container, including the relay
$ docker inspect … --format '{{json .NetworkSettings.Ports}}'
{"8080/tcp":null}       # web: EXPOSE only, no host mapping
{"80/tcp":null}         # backend: whoami's own EXPOSE
{}                      # relay
```

The only host listener this Run owns is the Surface forwarder, on loopback:

```text
$ lsof -nP -iTCP -sTCP:LISTEN | awk '$1=="ato"'
ato 43057 … TCP 127.0.0.1:50388 (LISTEN)
```

No listener on 8080 or 8081 belongs to this Run at any point.

### State

```text
backend mount: bind …/ato-home/cache/ato-portable-run-XXXX/state/data -> /data rw=true
web mount:     bind …/ato-home/cache/ato-portable-run-XXXX/workspace   -> /app   rw=false
relay mounts:  (none)
```

The state `data` slot is a read-write bind from the Run's own scratch, which
`ato pack` proved is inside Docker Desktop's shared directories. `ato run` is
ephemeral: the Run's scratch (and with it the state directory) is removed when
the Run stops. A durable seal belongs to the Instance lifecycle
(`ato app`/local Instance), not to `ato run`; that path was not re-verified
here.

### After stop

```text
containers: (none)
networks:   (none)
```

The relay image is kept (`ato-oci-relay:489d2967d873`, digest
`sha256:27764d7c…`): it is content-addressed and identical for the same
architecture and relay binary, so keeping it only saves a `docker load`.

## Preflight with the host unable to run the route (Phase B, condition E)

Each of the three cases below makes `ato pack` **succeed** (exit 0) and report
`runnable=no` with a reason; `ato run` fails before realizing anything, with the
same code and an action.

| Situation | `ato pack` | `ato run` |
|---|---|---|
| Docker stopped / socket missing (`DOCKER_HOST=unix:///nonexistent/docker.sock`) | `runnable=no reason=docker_daemon_unreachable` | fails before realize: "… no daemon answered (…); start Docker Desktop or Docker Engine and re-run" |
| Docker CLI absent (`PATH=/usr/bin:/bin`) | `runnable=no reason=docker_binary_missing` | fails before realize: "the docker CLI is not on PATH, so no container route can run here; install Docker …" |
| Remote daemon (`DOCKER_HOST=tcp://10.255.255.1:2375`) | `runnable=no reason=remote_daemon_workspace_unsupported` | fails before realize: "… this Run's workspace cannot be delivered there …" |

## What was not verified

- **Linux native Docker (condition D).** The available Linux host
  (`oci-linux-test`, Ubuntu aarch64) has no `docker` CLI (only `podman`) and its
  root filesystem is 100% full (0 bytes free), so the same bundle bytes could
  not be run there. The Direct connector path is covered by the adapter's
  existing Linux tests, but the end-to-end `fully_satisfied = true` +
  `connector=direct` receipt on Linux is **not** evidence in this page.
- **Rootless Docker.** Reasoned and unit-tested (`docker_bridge_unreachable_rootless`
  → relay), not run on a rootless host.
- **Docker Desktop file sharing for a path outside the shared directories.**
  Preflight refuses it (`docker_workspace_not_shared`) and that is unit-tested,
  but no real run was attempted with such a path.
- **A relay that outlives the Run.** The stop path verifies the container's
  absence; the "still exists → unconfirmed" branch is unit-level only.

## Reproducing the relay binary

```sh
scripts/build-oci-relay.sh   # writes extensions/adapters/oci/relay-bin/linux-{arm64,amd64}/ato-oci-relay
```

Requires `zig` and the musl targets for the workspace toolchain
(`rustup target add aarch64-unknown-linux-musl x86_64-unknown-linux-musl`).
The binaries are committed because the CLI embeds them with `include_bytes!`.

---

## 日本語要約

- macOS + Docker Desktop で `samples/oci-service-group-proof` を
  `ato pack` → `ato run` し、`fully_satisfied = true`、2 サービスの
  container_id と image、`connector = relay`、relay の digest と
  container_id、`platform_emulated = true` を receipt（schema /4）で確認した。
- 内部 Port は host に公開されていない（`docker port` は空、
  `HostConfig.PortBindings` は `{}`、`NetworkSettings.Ports` は EXPOSE のみ）。
  host 側の listener は Surface の `127.0.0.1` だけ。
- 停止後にコンテナも network も残らない。relay image は content-addressed
  なので保持する。
- Docker 停止・CLI 無し・remote context の 3 ケースで、`ato pack` は成功し
  理由付きで `runnable=no` を表示、`ato run` は realize 前に具体的な対処付きで
  失敗する。
- Linux native Docker での同一 bundle 実行は、検証機に docker が無く
  ディスクも満杯のため **未検証**。

## Review response (round 2)

The first draft of this change was reviewed. These are the findings and what
changed.

### R1 — a direct Run must keep the receipt stg produced

`execution.connector` was recorded for every Run, so every OCI Service Group
receipt became schema `/4`, while `accept_receipt` and the hosted validator
still accepted only `/1`–`/3`. Now:

- Connector evidence (connector, relay image digest, relay container id,
  platform emulated) is recorded **only for the relay materialization**. A
  direct Run's receipt is the receipt stg produced.
- The schema is one decision, highest statement first: connector `/4`,
  certificate `/3`, portability profile `/2`, base `/1`. A certificate no
  longer downgrades a `/4`.
- `accept_receipt` accepts `/4` and rejects connector evidence on `/1`–`/3`.
- Regression tests: `a_direct_service_group_receipt_keeps_the_stg_schema`,
  `a_relayed_receipt_is_v4_and_certificate_evidence_does_not_lower_it`,
  `a_relayed_receipt_is_v4_and_accepted`, `connector_evidence_requires_v4`.
- ato-api: a Draft PR teaches the hosted receipt schema `/4`
  (`ato-run/ato-api#1134`). Its CI could not start (billing), see below.

### R3 — layering

`arch-check` refused `ato-adapter-oci (adapter)` → `ato-host-facts (lib)` and
→ `ato-oci-relay (lib)`. Both are gone:

- The adapter normalises the daemon architecture itself; the caller passes the
  observation. No adapter depends on host facts.
- The relay wire format is the contract between the CLI and the relay
  container, so its crate is classified `adapter-api` (an adapter may depend on
  an adapter API) rather than `lib`. The rule itself was not relaxed.

### R4 — the embedded relay binaries have provenance

- `extensions/adapters/oci/relay-bin/SHA256SUMS` records the hash of both
  binaries; a test hashes what `include_bytes!` embedded and refuses a match
  that disagrees.
- `scripts/build-oci-relay.sh` pins the toolchain (1.96.0) and zig (0.15.2).
- The `relay-binaries` CI job rebuilds from source on the runner that defines
  the canonical bytes and fails unless the rebuilt SHA256SUMS equals the
  committed one. A binary built on another host is equivalent but not
  byte-identical, so the committed binaries are the ones this job produced.

### R9 — the tests that looked flaky

- `lib/runtime-attempt/src/data_plane.rs` reported `model_cache_busy` for any
  `flock` error, including `EINTR`. Only `EWOULDBLOCK` is contention now; an
  interrupted syscall is retried and other errors keep their own code.
- The Chrome/CDP tests (`native_probe_owner`, the worker's browser e2e) passed
  on the same branch at another commit and failed on Windows/Linux runs; the
  reruns and their run ids are recorded in the PR bodies.

## Linux direct acceptance (the other materialization)

`apps/cli/tests/oci_service_group_direct.rs` runs the same sample on a native
Linux Docker daemon and pins the macOS identity:

```text
direct E2E ok: bundle=sha256:2c88d86cc1c10039a9454b57ad0962986fabb0aa951970af9567c6b4dcb0a902
               contract=sha256:68228e9bf2bcadc375cc70747bb23e672290a1a16774d2ed6df387488db5e529
               derivation=sha256:a6ff45c1fb10cb349003b0703a0b4fdf7633879a7af361b9e04997514108148c
               schema="ato.contract-verification-receipt/1"
```

Same bundle bytes, same `ContractRef`, same `DerivationRef`; the receipt is the
pre-/4 schema because a direct Run records no connector. The CI Linux job runs
it with `ATO_REQUIRE_DOCKER_E2E=1`, so a runner without Docker fails instead of
skipping.

## macOS acceptance, re-run after the fixes

The relay binaries changed (CI-built), so the acceptance was re-run:

```text
fully_satisfied: true
schema: ato.contract-verification-receipt/4
connector: relay
relay_image_digest: sha256:2449d4dbbb436911cca8ef7f8d3a657004b4f0b8997354b6fba97a95deea0ea3
```

`accept_receipt` was run against that exact file with the bundle's own Contract
and the Run's attempt id; it is accepted. No internal Port was published and no
container or network remained after the stop (same checks as above; the run's
receipt is `.tmp/1690/mac.json`).
