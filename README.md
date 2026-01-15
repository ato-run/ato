<p align="center">
	<img src="docs/nacelle_logo.png" alt="nacelle" height="96">
</p>

# Nacelle

[![Rust](https://img.shields.io/badge/language-Rust-orange)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Docs](https://img.shields.io/badge/docs-Build.md-green)](docs/BUILD.md)
[![Security](https://img.shields.io/badge/security-Policy-red)](SECURITY.md)

**Nacelle** is a lightweight, self-contained runtime engine for executing containerized applications (Capsules) with built-in sandboxing, supervisor mode, and socket activation.

## 🚀 Quick Start

### Installation

```bash
# Clone the repository
git clone https://github.com/capsuled-dev/nacelle.git
cd nacelle

# Build the CLI
cargo install --path ./cli

# Verify installation
nacelle --version
```

### Run a Sample

```bash
cd samples/simple-todo
nacelle package ./capsule.toml --output app.capsule

# Deploy and run
nacelle deploy app.capsule
```

## ✨ Features

- **Self-Contained Bundles** — Single binary execution with zero external runtime dependencies
- **Supervisor Mode** — Robust process management, signal handling, and cleanup
- **Socket Activation** — Efficient port binding and file descriptor passing
- **Sandboxing** — Linux (eBPF-based) and macOS (Seatbelt) isolation
- **Multi-Language Support** — Python, Node.js, Ruby, Go, Rust, and more
- **gRPC API** — Programmatic capsule deployment and management
- **Cryptographic Signing** — Ed25519-based capsule verification

## 📋 Core Concepts

### What is a Capsule?

A **Capsule** is a portable, signed application package containing:

- **Manifest** (`capsule.toml`) — Application metadata, runtime config, and security policies
- **Application Code** — Source files (Python, Node.js, etc.) or compiled binaries
- **Resources** — Configuration files, assets, and dependencies
- **Signature** — Cryptographic proof of integrity and authenticity

### Architecture Overview

```
┌─────────────────────────────────────┐
│      Capsule CLI / Meta Layer       │
│  (Package, Verify, Orchestrate)     │
└────────────────┬────────────────────┘
                 │ calls
                 ▼
┌─────────────────────────────────────┐
│   Nacelle Engine (This Repository)  │
├─────────────────────────────────────┤
│  ├─ Supervisor (Process Management) │
│  ├─ Socket Activation               │
│  ├─ Sandbox (eBPF / Seatbelt)       │
│  └─ Runtime Execution               │
└─────────────────────────────────────┘
```

## 📚 Documentation

- [Build Guide](docs/BUILD.md) — Compilation, cross-compilation, and eBPF/Protobuf setup
- [Engine Interface Contract](docs/ENGINE_INTERFACE_CONTRACT.md) — IPC protocol specification
- [Security Policy](SECURITY.md) — Vulnerability reporting and security guidelines
- [Contributing Guide](CONTRIBUTING.md) — Code style, PR process, and development workflow
- [Sample Applications](samples/README.md) — Example capsules (Python, Node.js)

## 🔧 Building from Source

### Prerequisites

- **Rust 1.82+** (2021 edition)
- **Linux (for eBPF):** `llvm-14`, `clang-14`, `linux-headers`
- **macOS:** Xcode Command Line Tools
- **Protoc:** `brew install protobuf` (macOS) or `apt install protobuf-compiler` (Linux)

### Development Build

```bash
# Build library and CLI
cargo build

# Run tests
cargo test --lib

# Format and lint
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

### Release Build

```bash
# Build optimized binaries
cargo build --release

# Install CLI globally
cargo install --path ./cli
```

## 🔒 Security

- **No Hardcoded Secrets** — Keys are dynamically generated or loaded from secure vaults
- **Cryptographic Signing** — All capsules are signed with Ed25519 keys
- **Sandbox Isolation** — Runtime enforces strict resource and network policies
- **Audit Logging** — All deployments and executions are logged

**Found a vulnerability?** See [SECURITY.md](SECURITY.md) for responsible disclosure guidelines.

## 🌍 Samples & Examples

| Sample | Language | Type | Description |
|--------|----------|------|-------------|
| [simple-todo](samples/simple-todo/) | Python/React | Full Stack | TODO app with React frontend + Python backend |
| [my-app](samples/my-app/) | TypeScript | Backend | Node.js/Bun-based API service |

Run samples locally:

```bash
cd samples/simple-todo
nacelle dev --manifest capsule.toml
```

## 📋 Project Structure

```
nacelle/
├── src/
│   ├── engine/         # Process supervision and lifecycle
│   ├── runtime/        # Language runtimes (Python, Node.js, etc.)
│   ├── resource/       # Artifact management and CAS
│   ├── common/         # Shared types and utilities
│   ├── security/       # Policy validation and sandboxing
│   └── observability/  # Metrics, audit, and logging
├── cli/                # Command-line interface
├── proto/              # gRPC service definitions
├── samples/            # Example capsules
├── docs/               # Architecture and developer guides
└── tests/              # Integration tests
```

## 🛠️ Common Tasks

### Create a New Capsule

```bash
mkdir my-app
cd my-app

# Create manifest
cat > capsule.toml <<EOF
schema_version = "1.0"
name = "my-app"
version = "0.1.0"

[execution]
runtime = "source"
language = "python"
entrypoint = "app.py"
EOF

# Create application
echo 'print("Hello, Capsule!")' > app.py

# Package
nacelle package ./capsule.toml --output my-app.capsule
```

### Deploy with Signature

```bash
# Generate signing key
nacelle keygen my-key

# Sign capsule
nacelle sign my-app.capsule --key ~/.capsule/keys/my-key.secret

