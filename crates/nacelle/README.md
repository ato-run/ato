# nacelle

`nacelle` は **Capsule を実行するためのエンジン（internal runtime）**です。エンドユーザーが直接触る入口は `ato-cli` を想定し、`ato-cli` がプロセス境界（JSON over stdio）で `nacelle internal ...` を呼び出して実行します。

## 役割

- **Mechanism（実行メカニズム）**
  - バンドル/アーティファクトの展開
  - OSネイティブ隔離（filesystem / network, macOS は Seatbelt ベース）
  - プロセス起動・監視（Supervisor Mode）
  - Socket Activation（FD継承）

- **非ゴール（原則ホスト側へ）**
  - 署名検証・ポリシー決定・対話的UX（Smart Build, Dumb Runtime）
  - OS API提供（Host Bridge Pattern はホスト側の責務）

## Performance

Nacelle does not start a container or virtual machine.

Benchmarks separate cached execution from first-run setup. Cached execution
means required tools are already available in the local cache.

Because `nacelle` is an internal engine, the source-workload rows below measure
the same `internal exec` path invoked by `ato-cli`.

Measured on macOS using the release build of Nacelle (`target/release/nacelle`)
and `hyperfine 1.20.0`.

| Scenario               |   Direct | Shell wrapper | Nacelle cached | Overhead vs direct |
| ---------------------- | -------: | ------------: | -------------: | -----------------: |
| no-op (`true`)         |   1.8 ms |           n/a |         7.4 ms |            +5.6 ms |
| source workload `true` |   1.8 ms |           n/a |         8.8 ms |            +7.0 ms |
| `node --version`       |   9.1 ms |       15.0 ms |        34.2 ms |           +25.1 ms |
| `pnpm --version`       | 144.6 ms |      151.6 ms |       177.4 ms |           +32.8 ms |
| `pnpm build`           |  1.392 s |       1.391 s |        1.428 s |     +36 ms / ~2.6% |

Cached source workloads add around 7-33 ms of overhead in this benchmark suite.
For cached runs, short commands show Nacelle's fixed startup cost. For longer
commands, the cost is amortized by the underlying tool or build process.

### First-run setup

| Scenario                                                   | First run |
| ---------------------------------------------------------- | --------: |
| `node --version` with forced Node 20.19.4 JIT provisioning |  18.532 s |

First-run setup includes tool resolution, download, extraction, and cache
creation. It is reported separately from cached execution overhead.

### Benchmark notes

- Runtime-aware Node and pnpm benchmarks use manifest-based source workloads.
- A shell workload with `cmd = ["node", ...]` keeps `language = "shell"`, so it does not exercise the same runtime-aware execution path.

## Security boundary

Nacelle runs commands on the host and applies OS-native isolation mechanisms
where available.

It does not provide the same boundary as a container runtime or a full virtual
machine. Use Docker or a VM when you need a stronger security boundary.

## How Nacelle compares

| Tool                 | Runs in   |       Isolation | Startup cost | Best for                  |
| -------------------- | --------- | --------------: | -----------: | ------------------------- |
| Direct command       | host      |            none |       lowest | local commands            |
| Shell script         | host      |            none |          low | simple automation         |
| mise / asdf          | host      |             low |          low | tool version management   |
| npx / pnpm dlx / uvx | host      |             low |       medium | package-provided tools    |
| Nacelle              | host      | low / OS-native |          low | project-defined execution |
| Docker               | container |     medium/high |  medium/high | isolated services         |
| VM / Vagrant         | full OS   |            high |         high | full development machines |

Nacelle is closest to a project execution launcher: it resolves runtimes,
tools, and execution metadata for host-based source workloads without turning
the project into a container image or a virtual machine.

## ドキュメント

- Engine契約（CLI↔Engine）: [nacelle/docs/ENGINE_INTERFACE_CONTRACT.md](docs/ENGINE_INTERFACE_CONTRACT.md)
- セキュリティポリシー: [nacelle/SECURITY.md](SECURITY.md)
- 最新アーキテクチャ概要（repo全体）: [docs/architecture/ARCHITECTURE_OVERVIEW.md](../docs/architecture/ARCHITECTURE_OVERVIEW.md)

## 関連ADR

- `docs/adr/2026-01-06_000000_smart-build-dumb-runtime.md`
- `docs/adr/2026-01-03_000000_supervisor-mode.md`
- `docs/adr/2026-01-15_000001_socket-activation.md`
- `docs/adr/2026-01-07_000000_system-abstraction.md`

## License

MPL-2.0.
