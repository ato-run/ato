# Nacelle Sample Applications

This directory contains example applications demonstrating various approaches to building and deploying with Nacelle Capsules.

## 📋 Overview

| Sample | Language | Type | Description | Build |
|--------|----------|------|-------------|-------|
| **[simple-todo](#simple-todo)** | Python/React | Full Stack | TODO app with React frontend and Python runtime | `./build.sh` |
| **[my-app](#my-app)** | TypeScript | Backend | Node.js/Bun-based application | `bun install && bun run dev` |

---

## Simple TODO

**Path:** `./simple-todo/`

A minimal yet functional TODO application showcasing Capsule deployment with a React frontend and Python backend.

### Features

- ✅ React + Vite frontend
- 🐍 Python runtime entry point
- 📦 Capsule manifest (`capsule.toml`)
- 🔒 Containerized deployment
- 💾 Local state management

### Prerequisites

- Node.js 18+ and pnpm
- Python 3.9+
- Nacelle CLI tools (`cargo install --path ./cli`)

### Quick Start

```bash
cd samples/simple-todo

# Build the React application
./build.sh

# Verify the Capsule manifest
nacelle verify ./capsule.toml

# Package as a Capsule
nacelle package ./capsule.toml --output simple-todo.capsule
```

### Project Structure

```
simple-todo/
├── capsule.toml       # Capsule manifest (declarative configuration)
├── app.py             # Python entry point
├── build.sh           # Build script for React app
├── package.json       # Node.js dependencies (React + Vite)
└── README.md          # Detailed documentation
```

### Key Files

- **`capsule.toml`**: Defines the application container, runtime, resources, networking, and security policies
- **`app.py`**: Python WSGI application or standalone server invoked by the Capsule runtime
- **`build.sh`**: Compiles the React frontend into `dist/`

---

## My App

**Path:** `./my-app/`

A TypeScript/Bun-based application demonstrating backend service deployment.

### Features

- 🚀 TypeScript with Bun runtime
- ⚡ Hot reload development mode
- 📦 Capsule manifest with multiple execution profiles
- 🔧 Minimal configuration

### Prerequisites

- Bun 1.0+
- Nacelle CLI tools

### Quick Start

```bash
cd samples/my-app

# Install dependencies
bun install

# Run in development mode
bun run dev

# Build Capsule
nacelle package ./capsule.toml --output my-app.capsule
```

### Project Structure

```
my-app/
├── capsule.toml       # Capsule manifest with Bun runtime
├── package.json       # Dependencies
├── tsconfig.json      # TypeScript configuration
├── bun.lockb          # Bun lock file (reproducible builds)
├── src/
│   └── index.ts       # Application entry point
└── README.md          # Project-specific documentation
```

### Key Files

- **`capsule.toml`**: Specifies Bun runtime and execution profiles (dev, release)
- **`src/index.ts`**: TypeScript application logic
- **`bun.lockb`**: Locked dependency versions for reproducibility

---

## Test Keys

**Path:** `./keys/`

Ephemeral test keys for development and CI environments.

⚠️ **These are test keys only. Never use in production.**

### Generate Test Keys

```bash
cd samples/keys

# Generate a new test keypair
./generate_test_key.sh e2e-test.json

# Use in tests
export CAPSULE_TEST_KEY="$(cat e2e-test.json)"
cargo test
```

For production key generation, use:
```bash
nacelle keygen my-production-key
```

See [keys/README.md](./keys/README.md) for details.

---

## Getting Started

### 1. Prerequisites

Ensure you have the Nacelle toolchain installed:

```bash
cd ../..  # Navigate to repo root
cargo install --path ./cli
```

### 2. Choose a Sample

Pick a sample that matches your use case:
- **Python developers**: Start with `simple-todo`
- **TypeScript/Node developers**: Start with `my-app`

### 3. Follow Sample-Specific Instructions

Each sample has its own `README.md` with detailed setup steps.

### 4. Build & Package

```bash
# Navigate to sample directory
cd samples/simple-todo  # or my-app

# Follow the "Quick Start" section in this README or the sample's own README
```

---

## CI/CD Integration

All samples are automatically tested in CI. To run locally:

```bash
# From repo root
./scripts/test-samples.sh
```

Each sample must:
- ✅ Build successfully
- ✅ Pass a smoke test (e.g., CLI invocation, health check)
- ✅ Produce a valid Capsule artifact

---

## Adding New Samples

To add a new sample:

1. Create a directory under `samples/`
2. Include:
   - `README.md` (with features, prerequisites, quick start)
   - `capsule.toml` (valid Capsule manifest)
   - Application code (`.py`, `.ts`, `.js`, etc.)
   - Build/run script (`build.sh`, `Makefile`, or equivalent)
3. Add a `.gitignore` entry if needed
4. Update this `README.md` with a summary row

Samples should be **minimal**, **self-contained**, and **focus on a single concept**.

---

## Support

For questions or issues:

- 📖 See individual sample `README.md` files
- 🐛 Open an issue on the main repository
- 💬 Refer to the main [README.md](../README.md) for general help

---

## License

All samples are provided under the same license as the Nacelle project. See [LICENSE](../LICENSE) for details.
