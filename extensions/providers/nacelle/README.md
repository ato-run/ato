# nacelle

`nacelle` is the **engine (internal runtime) for running Capsules**. End users
are expected to use `ato-cli` as their entry point; `ato-cli` invokes
`nacelle internal ...` across a process boundary (JSON over stdio) to execute a
workload.

## Role

- **Mechanism (execution mechanism)**
  - Unpacking bundles and artifacts
  - OS-native isolation (filesystem and network; Seatbelt-based on macOS)
  - Process launch and supervision (Supervisor Mode)
  - Socket Activation (FD inheritance)

- **Non-goals (stay on the host side)**
  - Signature verification, policy decisions and interactive UX (Smart Build, Dumb Runtime)
  - Providing OS APIs (the Host Bridge Pattern is the host's responsibility)

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

## Documentation

- Engine contract (CLI ↔ engine): [nacelle/docs/ENGINE_INTERFACE_CONTRACT.md](docs/ENGINE_INTERFACE_CONTRACT.md)
- Security policy: [nacelle/SECURITY.md](SECURITY.md)
- Current architecture overview (whole repository): [Ato architecture](../../../docs/architecture/README.md)

## Related ADRs

- [`docs/adr/0001-cli-entrypoint.md`](docs/adr/0001-cli-entrypoint.md): CLI entry point for `nacelle`
- [`docs/adr/ADR-oci-local-ingress.md`](docs/adr/ADR-oci-local-ingress.md): local ingress for OCI service endpoints

## License

MPL-2.0.
