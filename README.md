# capsuled

**Capsule Application Runtime Engine** - UARC V1.1.0 準拠のランタイム実装

[![UARC](https://img.shields.io/badge/UARC-V1.1.0-blue)](../uarc/SPEC.md)
[![License](https://img.shields.io/badge/license-FSL--1.1--ALv2-green)](LICENSE)

## Overview

capsuled は [UARC (Universal Application Runtime Contract)](../uarc/SPEC.md) V1.1.0 仕様に完全準拠した、セキュアなアプリケーションランタイムエンジンです。複数のランタイム (Wasm, Source, OCI) をサポートし、CAS-based verification、SPIFFE ID ベースのネットワーク認証、GPU セキュリティなどの先進的な機能を提供します。

### UARC V1.1.0 準拠

✅ **Supported Runtimes**:

- **Wasm**: WebAssembly サンドボックス実行
- **Source**: インタープリタ言語 (Python, Node.js, Ruby, etc.)
- **OCI**: コンテナランタイム (Youki, Docker)

✅ **Security Features**:

- CAS-based resource verification (SHA256)
- SPIFFE ID network identity (SVID authentication)
- Path validation & egress policy enforcement
- GPU VRAM scrubbing (multi-tenant isolation)

✅ **Architecture Compliance**:

- Layer-based design (L1-L5)
- Capsule manifest verification
- Service discovery & registration
- Audit logging & observability

## Features

### Core Capabilities

- **Multi-Runtime Support**: Wasm, Source (interpreted languages), OCI containers
- **Secure Execution**: Signature verification, CAS integrity checks, isolated environments
- **Resource Management**: Generic resource ingestion with SHA256 verification
- **Network Security**: SPIFFE ID-based peer authentication, egress proxy
- **Storage Management**: LVM-based volume provisioning, CAS artifact storage
- **GPU Support**: VRAM security scrubbing for multi-tenant workloads
- **Service Discovery**: mDNS announcer for development environments
- **Observability**: Prometheus metrics, audit logging, structured tracing

### API

- **gRPC Server**: Full UARC-compliant API for Capsule lifecycle management
- **Dev Server**: Development-optimized runtime with hot-reload support
- **CLI Tools**: Capsule build, deploy, and management utilities

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                     capsuled                            │
├─────────────────────────────────────────────────────────┤
│  Interface Layer (L5)                                   │
│  ├─ gRPC Server                                         │
│  ├─ Dev Server                                          │
│  └─ Discovery (mDNS)                                    │
├─────────────────────────────────────────────────────────┤
│  Engine Layer (L4)                                      │
│  ├─ Capsule Manager                                     │
│  ├─ Service Registry                                    │
│  └─ Manifest Verifier                                   │
├─────────────────────────────────────────────────────────┤
│  Runtime Layer (L3)                                     │
│  ├─ WasmRuntime                                         │
│  ├─ SourceRuntime / DevRuntime                          │
│  └─ YoukiRuntime / DockerCliRuntime                     │
├─────────────────────────────────────────────────────────┤
│  Resource Layer (L2)                                    │
│  ├─ Resource Ingestion (HTTP/S3)                        │
│  ├─ Artifact Manager (CAS)                              │
│  └─ Storage Manager (LVM)                               │
├─────────────────────────────────────────────────────────┤
│  Common Layer (L1)                                      │
│  ├─ Security (Path Validation)                          │
│  ├─ Verification (VRAM, Signature)                      │
│  └─ Observability (Metrics, Audit)                      │
└─────────────────────────────────────────────────────────┘
```

## Building

### Prerequisites

- Rust 1.83+ (2021 edition)
- Protocol Buffers compiler (`protoc`)
- (Optional) CUDA toolkit for GPU support
- (Optional) LVM tools for storage management

### Compile

```bash
cargo build --release
```

### Development Build

```bash
cargo build
```

### Run Tests

```bash
cargo test
```

## Usage

### Start Runtime Engine

```bash
# Production mode
./target/release/capsuled --config config.toml

# Development mode with auto-reload
./target/release/capsuled --dev-server --grpc-port 8080
```

### Configuration

Create `config.toml`:

```toml
[runtime]
kind = "youki"  # or "docker", "source", "wasm"
binary_path = "/usr/local/bin/youki"
state_root = "/var/run/capsuled"

[security]
allowed_host_paths = ["/tmp", "/data"]
egress_proxy_port = 3128

[storage]
default_vg = "capsuled-vg"
```

See [config.toml.example](config.toml.example) for full configuration options.

## Runtime Selection

Capsuled automatically selects the appropriate runtime based on manifest:

- **Wasm**: `runtime.type = "wasm"` → `WasmRuntime`
- **Source**: `runtime.type = "source"` → `SourceRuntime` or `DevRuntime`
- **OCI**: `runtime.type = "oci"` or `runtime.type = "docker"` → `YoukiRuntime` (Linux) or `DockerCliRuntime` (macOS)

### Legacy Compatibility

Native runtime manifests are automatically migrated to Source runtime:

```toml
# Legacy (auto-converted)
[runtime]
type = "native"
binary_path = "/usr/bin/my-app"

# Converts to:
[runtime]
type = "source"
language = "generic"
cmd = ["/usr/bin/my-app"]
```

## Development

### Project Structure

```
capsuled/
├── src/
│   ├── interface/      # L5: gRPC, DevServer, Discovery
│   ├── engine/         # L4: CapsuleManager, ServiceRegistry
│   ├── runtime/        # L3: Wasm, Source, OCI runtimes
│   ├── resource/       # L2: Ingestion, Artifacts, Storage
│   ├── common/         # L1: Proto, Types, Contracts
│   ├── security/       # Path validation, Access control
│   ├── verification/   # Signature, VRAM scrubbing
│   └── observability/  # Metrics, Audit, Tracing
├── proto/              # gRPC protocol definitions
└── docs/               # Architecture & implementation docs
```

### Key Documents

- [UARC_SCOPE_REVIEW.md](UARC_SCOPE_REVIEW.md) - Scope analysis and compliance review
- [PHASE13_COMPLETION.md](PHASE13_COMPLETION.md) - Native runtime removal report
- [MIGRATION_SUMMARY.md](MIGRATION_SUMMARY.md) - Migration guide from legacy architecture
- [PROJECT_OVERVIEW.md](PROJECT_OVERVIEW.md) - High-level architecture overview

## UARC V1.1.0 Compliance

Capsuled は以下の UARC 仕様要件を満たしています:

### ✅ Supported

- Wasm Runtime (wasmtime-based)
- Source Runtime (Python, Node.js, Ruby, Deno, etc.)
- OCI Runtime (Youki, Docker)
- CAS-based artifact verification
- SPIFFE ID network identity
- Path validation & egress policy
- Service discovery & registration
- Metrics & audit logging

### ❌ Explicitly Excluded (UARC V1 Non-Compliance)

- **Native Runtime**: Archived (security concerns) - use Source Runtime instead
- **Tailscale/Headscale VPN**: Archived - use SPIFFE ID for peer authentication
- **Traefik Routing**: Archived - Coordinator responsibility, not Engine scope

See [UARC SPEC.md](../uarc/SPEC.md) for detailed specification.

## License

FSL-1.1-ALv2 (Functional Source License 1.1, Apache License Version 2.0)

## Contributing

1. Read [UARC SPEC.md](../uarc/SPEC.md) to understand the architecture
2. Check [UARC_SCOPE_REVIEW.md](UARC_SCOPE_REVIEW.md) for scope guidelines
3. Follow Rust best practices and maintain UARC compliance
4. Add tests for new features
5. Update documentation

## Related Projects

- [ato-coordinator](../ato-coordinator/) - Cluster orchestration & routing
- [ato-desktop](../ato-desktop/) - Desktop UI for Capsule management
- [capsule-cli](../capsule-cli/) - Capsule build & deployment tools
- [uarc](../uarc/) - UARC specification and protocol definitions
