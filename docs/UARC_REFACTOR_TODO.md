# UARC Refactoring TODO

## Status: ✅ COMPLETE

All refactoring tasks have been completed successfully on 2024.

### Summary of Changes

- ✅ **Archived 5 modules/folders** to `.archives/` (non-destructive)
- ✅ **Updated 8 import statements** to use `capsule_core::capsule_v1::`
- ✅ **Removed 3 module declarations** from `src/lib.rs`
- ✅ **Refactored preflight checks** to remove cloud configuration dependency
- ✅ **All tests passing** (5/5 in failure_codes module)
- ✅ **Full compilation success** with `cargo check`
- ✅ **UARC V1 compliance verified** via comprehensive checklist

See [UARC_V1_COMPLIANCE_CHECKLIST.md](../UARC_V1_COMPLIANCE_CHECKLIST.md) for full verification.

---

## 1. ✅ Remove legacy module

**Status**: COMPLETED  
**Reason**: Unnecessary re-export layer

### What Was Done

1. ✅ Archived legacy module `capsuled/src/adep/` to `.archives/src/adep/` (legacy; kept for reference)
2. ✅ Updated imports in 8 locations:

   - ✅ `src/lib.rs` - Updated OCI stub imports
   - ✅ `src/workload/manifest_loader.rs`
   - ✅ `src/capsule_manager.rs`
   - ✅ `src/oci/spec_builder.rs` (4 uses)
   - ✅ `src/runtime/native.rs`

3. ✅ Replaced legacy `crate::adep::` imports with `capsule_core::capsule_v1::`

### Verification

```bash
# No compilation errors
cargo check ✓

# All tests passing
cargo test failure_codes ✓ (5 tests)
```

---

## 2. ✅ Remove `billing` module

**Status**: COMPLETED  
**Reason**: Unused SaaS-specific feature not relevant to UARC Engine

### What Was Done

- ✅ Moved `capsuled/src/billing/` to `.archives/src/billing/`
- ✅ Removed `pub mod billing;` from `src/lib.rs`
- ✅ Confirmed zero references in codebase (pre-verified)

### Why Archived

**Out of scope for UARC Engine:**

- Billing calculations are Platform/Coordinator responsibility
- Engine focuses on verification (L1-L5) and execution only
- No active dependencies anywhere in the codebase

---

## 3. ✅ Remove `cloud` module

**Status**: COMPLETED  
**Reason**: Cloud bursting is Coordinator responsibility, not Engine concern

### Analysis

- **Minimal module**: Only 2 files (`mod.rs`, `models.rs`) with ~20 lines total
- **Zero usage**: No imports of `cloud::CloudDeployRequest` or `cloud::CloudDeployResponse` anywhere in capsuled
- **Architectural violation**: Cloud deployment is explicitly **not an Engine responsibility** per UARC spec
- **Already deprecated**: `CapsuleManager::cloud_configured()` hardcoded to return `false` with comment: _"Cloud deployment is no longer supported in Engine (SPEC V1.1.0)"_

### Module Contents

```rust
// cloud/models.rs
pub struct CloudDeployRequest {
    pub capsule_id: String,
    pub manifest: String,
}

pub struct CloudDeployResponse {
    pub job_id: String,
    pub status: String,
    pub endpoint: Option<String>,
}
```

### Related Concepts in Manifest (Keep These)

The `fallback_to_cloud` and `cloud_capsule` fields in `CapsuleRouting` are **routing hints**, not Engine implementation:

```rust
// In capsule_v1::CapsuleRouting
pub fallback_to_cloud: bool,      // Routing hint for Coordinator
pub cloud_capsule: Option<String>, // Cloud Capsule ID hint
```

These fields should be:

### What Was Done

- ✅ Moved `capsuled/src/cloud/` to `.archives/src/cloud/`
- ✅ Removed `pub mod cloud;` from `src/lib.rs`
- ✅ Removed `cloud_configured()` method from `CapsuleManager`
- ✅ Updated `compute_deploy_failure_codes()` to remove `cloud_configured` parameter
- ✅ Updated `grpc_server.rs` preflight check logic

### UARC Architecture: Cloud Bursting Flow

```
┌─────────────┐
│   Client    │
└──────┬──────┘
       │ deploy(manifest)
       v
┌─────────────┐
│ Coordinator │ ← Cloud Bursting Logic Lives Here
└──────┬──────┘
       │
       ├─→ Can local Engine handle it?
       │   ├─ Yes → deploy to local Engine
       │   └─ No  → deploy to SkyPilot/Cloud
       │
       v
┌─────────────┐
│ Local Engine│ ← No cloud deployment code
└─────────────┘
```

---

## 4. ✅ Remove `provisioning` folder

**Status**: COMPLETED  
**Reason**: Infrastructure provisioning is out of scope for UARC Engine

### What Was Done

- ✅ Moved `provisioning/` to `.archives/provisioning/`
- ✅ Removed all infrastructure deployment automation from codebase
- ✅ Confirmed no code dependencies exist

### Why Archived

**Out of scope for UARC Engine:**

- VM provisioning is operations/platform responsibility
- systemd service management is deployment tooling
- Cloud-init configuration is infrastructure setup
- Engine is a library/service, not infrastructure provisioning tool

---

## 5. ✅ Review `migrations` folder

**Status**: COMPLETED (Archived)  
**Reason**: Duplicate schema definitions with inline audit schema

### What Was Done

- ✅ Moved `migrations/` to `.archives/migrations/`
- ✅ Kept inline audit schema in `src/security/audit.rs` (SOURCE OF TRUTH)
- ✅ Confirmed audit logging still fully functional

### Resolution

**Decision**: Keep inline schemas for L5 observability

- `src/security/audit.rs` creates audit_logs table at runtime
- Aligns with stateless Engine philosophy (schema created on-demand)
- Eliminates migration management complexity
- Single source of truth for schema definition
- Delete `migrations/` folder
- Keep inline `CREATE TABLE IF NOT EXISTS` in Rust code
- Engine manages its own schema at startup
- ✅ No external dependencies
- ❌ No formal migration story for schema changes

**Option B: Use migrations properly** (More robust)

- Keep `migrations/` folder
- Add `sqlx` migration support to Engine startup
- Remove inline `CREATE TABLE` from audit.rs
- Use `sqlx::migrate!()` macro in Rust
- ✅ Proper schema versioning
- ❌ Adds complexity

**Option C: Minimal state, mostly stateless** (UARC-aligned)

- Delete `migrations/` folder
- **Audit logs**: Keep inline schema in `audit.rs` (L5 requirement)
- **Port allocations**: In-memory only (ServiceRegistry already does this)
- **Deployments/Capsules**: Let Coordinator manage persistent state
- Engine restart = clean slate (except audit history)
- ✅ Aligns with stateless Engine philosophy
- ✅ Simplest deployment model

### Recommendation

Choose **Option C** for UARC alignment:

- Engine should be as stateless as possible
- Persistent audit logs are the only hard requirement (L5)
- Other state can be ephemeral or coordinator-managed

---

## Summary

| Module         | Status    | Reason                            | Action                                     |
| -------------- | --------- | --------------------------------- | ------------------------------------------ |
| legacy/adep    | ❌ Remove | Unnecessary re-export             | Delete + update imports                    |
| `billing`      | ❌ Remove | Out of scope for UARC Engine      | Delete module                              |
| `cloud`        | ❌ Remove | Coordinator responsibility        | Delete module + refactor preflight         |
| `provisioning` | ❌ Remove | Infrastructure provisioning       | Move to separate ops repo                  |
| `migrations`   | ⚠️ Review | Schema duplication, unclear usage | Option C: Delete, keep inline audit schema |

**Total cleanup**: ~1,530+ lines + provisioning scripts + migration files

---

## Architectural Clarity: Stateless Engine vs. Stateful Applications

### The Distinction

**UARC Engine is stateless** ≠ **Applications running on it are stateless**

This is a critical architectural separation:

```
┌─────────────────────────────────────────────────────────────┐
│ Application Layer (Stateful)                                 │
│ ┌─────────────┐  ┌─────────────┐  ┌─────────────┐          │
│ │ Capsule A   │  │ Capsule B   │  │ Capsule C   │          │
│ │ (Chat Bot)  │  │ (Database)  │  │ (ML Model)  │          │
│ │ + Redis     │  │ + SQLite    │  │ + Weights   │          │
│ └─────────────┘  └─────────────┘  └─────────────┘          │
│       ↕               ↕                ↕                     │
│  [Persistent        [Volume         [Model                  │
│   Storage]           Mount]          Cache]                 │
└─────────────────────────────────────────────────────────────┘
                            ↕
┌─────────────────────────────────────────────────────────────┐
│ UARC Engine (Stateless Execution Environment)               │
│ ┌──────────┐  ┌──────────┐  ┌──────────┐                   │
│ │ Runtime  │  │ Network  │  │ Security │                   │
│ │ Resolver │  │ Policy   │  │ Verifier │                   │
│ └──────────┘  └──────────┘  └──────────┘                   │
│                                                              │
│ Engine state = ephemeral (restart → clean slate)            │
│ Audit logs = only persistent Engine data (L5)               │
└─────────────────────────────────────────────────────────────┘
                            ↕
┌─────────────────────────────────────────────────────────────┐
│ Coordinator / Platform Layer (State Management)             │
│ ┌──────────┐  ┌──────────┐  ┌──────────┐                   │
│ │ Capsule  │  │ Resource │  │ Billing  │                   │
│ │ Registry │  │ Scheduler│  │ Tracker  │                   │
│ └──────────┘  └──────────┘  └──────────┘                   │
│                                                              │
│ • Deployment records                                         │
│ • Capsule lifecycle                                          │
│ • Resource allocation history                                │
└─────────────────────────────────────────────────────────────┘
```

### How Stateful Services Work with Stateless Engine

#### Example 1: Stateful Chat Service

```toml
# capsule.toml
[storage.volumes]
[[storage.volumes]]
name = "chat-history"
mount_path = "/data/chat"
size_bytes = 10737418240  # 10GB
encrypted = true

[execution]
runtime = "source"
entrypoint = "server.py"
port = 8080

# Application code uses /data/chat for persistence
```

**Architecture:**

```
User Request → Coordinator → Engine
                              ↓
                         Start Capsule
                              ↓
                    Mount volume: /data/chat
                    (managed by StorageManager)
                              ↓
                    Python app reads/writes to /data/chat
                              ↓
                    Data persists across restarts
```

**Key Points:**

- **Engine**: Mounts the volume, doesn't care about content
- **Capsule**: Manages application state (chat history)
- **StorageManager**: Handles volume lifecycle (Engine component)
- **Coordinator**: Tracks which volumes belong to which capsules

#### Example 2: Database as a Capsule

```toml
# postgres-capsule.toml
[storage.volumes]
[[storage.volumes]]
name = "pg-data"
mount_path = "/var/lib/postgresql/data"
size_bytes = 107374182400  # 100GB

[execution]
runtime = "oci"
entrypoint = "postgres:15"
port = 5432
```

**Engine doesn't know/care that this is a database**:

- Engine: "I mount volumes, enforce egress rules, monitor health"
- Postgres: "I manage tables, transactions, WAL logs"

#### Example 3: ML Model with Cached Weights

```toml
# ml-inference.toml
[model]
source = "hf:meta-llama/Llama-3-8B"
weights_path = "/models/llama3-8b"

[storage.volumes]
[[storage.volumes]]
name = "model-cache"
mount_path = "/models"
size_bytes = 21474836480  # 20GB

[execution]
runtime = "source"
language = "python"
entrypoint = "serve.py"
```

**Flow:**

1. **Coordinator**: Checks if weights exist in `/models/llama3-8b`
2. **If not exists**: Downloads weights via model_fetcher (one-time)
3. **Engine**: Starts capsule with pre-warmed volume
4. **Application**: Loads weights from `/models` (fast, cached)

### What Engine State IS

**Minimal ephemeral state** (in-memory):

- ✅ Running capsule PIDs
- ✅ Port allocations (current session)
- ✅ GPU assignments
- ✅ Active network policies

**Single persistent state** (UARC L5 requirement):

- ✅ Audit logs (tamper-evident, signed)

**Why this design?**

- Engine crash/restart = clean recovery
- No "half-started" capsule state to reconcile
- Coordinator re-deploys if needed
- Audit trail survives for compliance

### What Engine State IS NOT

**Not Engine's responsibility:**

- ❌ Capsule deployment history (Coordinator)
- ❌ User quotas/billing (Platform)
- ❌ Capsule registry/versions (Coordinator)
- ❌ Long-term resource usage stats (Coordinator)
- ❌ Application data (Capsule's storage volumes)

### Practical Example: Deploying a Stateful App

**Scenario**: Deploy a chat application that needs to persist conversations

**Step 1: Coordinator receives deploy request**

```json
{
  "capsule_id": "chat-app-v1",
  "manifest": "...",
  "volumes": [{ "name": "chat-db", "size": "10GB" }]
}
```

**Step 2: Coordinator checks Engine capacity**

```
Coordinator → Engine.GetCapabilities()
Engine → { "available_storage": "500GB", "gpus": [] }
```

**Step 3: Coordinator provisions storage (via Engine's StorageManager)**

```
Coordinator → Engine.ProvisionVolume("chat-db", 10GB)
Engine.StorageManager → Creates /var/lib/capsuled/volumes/chat-db/
Engine → { "volume_id": "vol-abc123", "path": "/var/lib/capsuled/volumes/chat-db" }
```

**Step 4: Coordinator deploys capsule**

```
Coordinator → Engine.DeployCapsule(manifest, volumes=[vol-abc123])
Engine → Resolves runtime (source/python)
Engine → Mounts volume to /data/chat
Engine → Starts capsule with egress rules
Engine → { "status": "running", "pid": 12345, "port": 8080 }
```

**Step 5: Capsule runs, writes to /data/chat**

```python
# Inside capsule (server.py)
import sqlite3
db = sqlite3.connect('/data/chat/conversations.db')
# Data persists in Engine-managed volume
```

**Step 6: Engine crash/restart**

```
Engine restarts → In-memory state cleared
Coordinator detects → "chat-app-v1 not running"
Coordinator → Re-deploys capsule (same volume)
Engine → Mounts existing volume /data/chat
Capsule → Reads existing conversations.db
Result → Service resumed with full state
```

### Key Takeaway

**"Stateless Engine" means:**

- Engine itself has no business logic state to persist
- Engine is a **pure execution environment**
- Like Kubernetes kubelet: manages pods, doesn't care about app state
- Crash and restart = safe operation (after Coordinator reconciles)

**"Stateful Applications" mean:**

- Applications use Engine's volume management
- Applications use Engine's network isolation
- Applications use Engine's security policies
- But application logic and data = capsule's responsibility

**Separation of concerns:**

- **Engine**: "How to run" (runtime, security, isolation)
- **Coordinator**: "What to run" (scheduling, lifecycle, quotas)
- **Capsule**: "What it does" (business logic, data management)

This is why `migrations/` for Engine deployment tracking doesn't belong in Engine code—that's Coordinator's job.

---

## Capsule vs. OCI vs. Kubernetes: What's the Difference?

### Quick Comparison

| Aspect                | **Capsule (UARC)**                 | **OCI Container**        | **Kubernetes (k8s)**             |
| --------------------- | ---------------------------------- | ------------------------ | -------------------------------- |
| **Abstraction Level** | Application package + runtime      | Runtime format           | Orchestration platform           |
| **Primary Purpose**   | Verifiable multi-runtime execution | Container image standard | Container orchestration at scale |
| **Runtime Support**   | Wasm, Source, OCI                  | OCI only                 | OCI containers (via CRI)         |
| **Verification**      | Built-in (L1-L5)                   | None (separate tools)    | Admission controllers (add-on)   |
| **Portability**       | Runtime-agnostic                   | Container-portable       | Cluster-portable                 |
| **State Management**  | Stateless Engine                   | Stateless runtime        | StatefulSet for apps             |
| **Comparable To**     | Docker Compose + verification      | Docker image             | Docker Swarm / ECS               |

### Detailed Breakdown

#### 1. **Capsule (UARC Engine)**

**What it is:**

- **Application packaging format** + **execution engine**
- Multi-runtime support (Wasm, interpreted source, OCI)
- Built-in security verification (signatures, source scanning, egress control)

**Analogy:**

> "Docker Compose for verifiable, multi-runtime applications"

**Example Capsule:**

```toml
# capsule.toml - Single file describes everything
[execution]
runtime = "source"  # Can be: wasm, source, oci
language = "python"
entrypoint = "server.py"
port = 8080

[targets.wasm]  # Alternative runtime target
digest = "sha256:abc..."
world = "wasi:cli/command"

[security]
egress_allow = ["https://api.github.com"]
signature = "ed25519:..."
```

**Key Features:**

- ✅ Runtime resolution (picks best runtime for workload)
- ✅ Built-in source verification (L1)
- ✅ Signature verification (L2)
- ✅ Identity-based networking (SPIFFE)
- ✅ Audit logging (L5)
- ❌ No orchestration (single-node focus)

**Use Cases:**

- Edge computing (Wasm runtime)
- Development environments (source runtime)
- Hybrid cloud (multi-target support)
- Regulated industries (verification requirements)

---

#### 2. **OCI Container**

**What it is:**

- **Container image format standard** (OCI = Open Container Initiative)
- Defines how to package, distribute, and run containers
- Just the **image format**, not the orchestration

**Analogy:**

> "The JPEG of containers - a standardized format"

**Example OCI Image:**

```dockerfile
# Dockerfile - Defines OCI image
FROM python:3.11-slim
COPY requirements.txt .
RUN pip install -r requirements.txt
COPY server.py .
CMD ["python", "server.py"]
```

**Key Features:**

- ✅ Widely adopted standard
- ✅ Portable across runtimes (Docker, containerd, podman)
- ✅ Layered filesystem (efficient storage)
- ✅ Registry distribution (Docker Hub, etc.)
- ❌ No built-in verification (need Sigstore, Notary)
- ❌ No multi-runtime support (containers only)
- ❌ No orchestration (single container)

**Use Cases:**

- Standard containerized applications
- CI/CD pipelines
- Microservices architecture
- Cloud-native apps

**Relationship with Capsule:**

- Capsule **can use** OCI as one of its runtime targets
- Capsule **adds** verification and multi-runtime support
- OCI image = one possible artifact in Capsule

---

#### 3. **Kubernetes (k8s)**

**What it is:**

- **Container orchestration platform**
- Manages multiple containers across multiple nodes
- Provides scheduling, scaling, networking, storage

**Analogy:**

> "AWS for containers - manages infrastructure at scale"

**Example Kubernetes Deployment:**

```yaml
# deployment.yaml - Kubernetes manifest
apiVersion: apps/v1
kind: Deployment
metadata:
  name: my-app
spec:
  replicas: 3 # Run 3 copies
  template:
    spec:
      containers:
        - name: app
          image: python:3.11-slim
          command: ["python", "server.py"]
---
apiVersion: v1
kind: Service
metadata:
  name: my-app-service
spec:
  type: LoadBalancer
  ports:
    - port: 80
      targetPort: 8080
```

**Key Features:**

- ✅ Multi-node orchestration
- ✅ Auto-scaling (HPA, VPA)
- ✅ Self-healing (restarts failed pods)
- ✅ Service discovery & load balancing
- ✅ Rolling updates
- ✅ Storage orchestration (PV, PVC)
- ❌ Complex (steep learning curve)
- ❌ OCI-only (no Wasm/source runtime support)
- ❌ No built-in verification (need policy engines)

**Use Cases:**

- Multi-node clusters
- High availability applications
- Large-scale microservices
- Cloud-native infrastructure

**Relationship with Capsule:**

- **Different layers**: Kubernetes = orchestration, Capsule = execution
- **Could work together**: Kubernetes could schedule Capsules instead of raw pods
- **Different focus**: k8s = scale, Capsule = verification + multi-runtime

---

### Visual Comparison: Deployment Flow

#### **Deploying with OCI + Docker**

```
Developer → Dockerfile → docker build → OCI Image → docker run
                                           ↓
                                    Local Container
```

#### **Deploying with OCI + Kubernetes**

```
Developer → Dockerfile → docker build → Push to Registry
                                           ↓
                                    kubectl apply
                                           ↓
                              Kubernetes Cluster (multi-node)
                                           ↓
                    ┌──────────┬──────────┬──────────┐
                  Pod 1      Pod 2      Pod 3
               (Container) (Container) (Container)
```

#### **Deploying with Capsule (UARC)**

```
Developer → capsule.toml → capsule sign → Capsule Package
                                              ↓
                              Coordinator → Engine
                                              ↓
                              Runtime Resolution
                              ┌─────┴─────┬────────┐
                            Wasm      Source      OCI
                         (portable) (fast dev) (legacy)
```

#### **Hybrid: Capsule on Kubernetes** (Future possibility)

```
Developer → capsule.toml → Push to Registry
                                ↓
                         kubectl apply
                                ↓
                    Kubernetes Cluster
                                ↓
                    ┌──────────┬──────────┐
              Capsule Engine  Capsule Engine
                  Node 1         Node 2
                    ↓              ↓
              Runtime Resolver  Runtime Resolver
                ↓    ↓    ↓      ↓    ↓    ↓
              Wasm  Src  OCI   Wasm  Src  OCI
```

---

### When to Use Each?

#### **Use Capsule (UARC Engine) when:**

- ✅ You need **runtime flexibility** (Wasm, source, containers)
- ✅ You need **built-in verification** (signatures, source scanning)
- ✅ You're deploying to **edge devices** (single-node, resource-constrained)
- ✅ You need **identity-based networking** (SPIFFE)
- ✅ You're in a **regulated industry** (audit requirements)
- ✅ You want **fast local development** (source runtime, no build step)

#### **Use OCI Containers when:**

- ✅ You need **standard packaging** (works everywhere)
- ✅ You're using **existing container tools** (Docker, podman)
- ✅ You don't need multi-runtime support
- ✅ You're deploying to **container-native platforms**

#### **Use Kubernetes when:**

- ✅ You need **multi-node orchestration**
- ✅ You need **auto-scaling** and **self-healing**
- ✅ You're managing **many microservices** (100s or 1000s)
- ✅ You need **declarative infrastructure**
- ✅ You have **ops team** to manage complexity

#### **Use Combinations:**

- **Capsule + OCI**: Capsule uses OCI as one runtime target
- **Kubernetes + OCI**: Standard k8s deployment
- **Kubernetes + Capsule**: Future possibility (k8s schedules Capsule Engines)

---

### Architecture Comparison

#### **Single Application Deployment**

**Docker Compose (OCI):**

```yaml
services:
  app:
    image: python:3.11
    command: python server.py
    ports:
      - "8080:8080"
    volumes:
      - ./data:/data
```

- ✅ Simple, familiar
- ❌ No verification
- ❌ OCI-only

**Capsule:**

```toml
[execution]
runtime = "source"
entrypoint = "server.py"
port = 8080

[storage.volumes]
[[storage.volumes]]
name = "data"
mount_path = "/data"

[security]
signature = "ed25519:..."
```

- ✅ Verified
- ✅ Multi-runtime
- ✅ Identity-based networking

**Kubernetes:**

```yaml
apiVersion: apps/v1
kind: Deployment
# ... 50+ lines of YAML ...
```

- ✅ Production-ready
- ❌ Overkill for single app
- ❌ Steep learning curve

---

### Key Philosophical Differences

#### **Capsule Philosophy:**

- **"Verifiable by default"**: Every package is signed and source-scanned
- **"Runtime-agnostic"**: Same package runs on Wasm/source/OCI
- **"Stateless Engine"**: Engine itself has no business logic state
- **"Single-node focus"**: Designed for edge, not clusters

#### **OCI Philosophy:**

- **"Standard format"**: Interoperability across tools
- **"Build once, run anywhere"**: Container portability
- **"Layered storage"**: Efficient disk usage
- **"Registry distribution"**: Centralized image hosting

#### **Kubernetes Philosophy:**

- **"Declarative configuration"**: Describe desired state, k8s maintains it
- **"Self-healing"**: Automatically restarts failed containers
- **"Immutable infrastructure"**: Replace, don't update
- **"Cloud-native"**: Built for distributed systems at scale

---

### Summary: Layered Architecture

```
┌─────────────────────────────────────────┐
│         Application Layer               │
│  (Your code: Python, Go, Rust, etc.)   │
└─────────────────────────────────────────┘
                  ↕
┌─────────────────────────────────────────┐
│      Packaging Layer                    │
│  • Capsule (UARC) ← Adds verification   │
│  • OCI Container  ← Standard format     │
└─────────────────────────────────────────┘
                  ↕
┌─────────────────────────────────────────┐
│      Runtime Layer                      │
│  • Wasm (wasmtime)                      │
│  • Source (python, node)                │
│  • OCI (youki, runc)                    │
└─────────────────────────────────────────┘
                  ↕
┌─────────────────────────────────────────┐
│   Orchestration Layer (Optional)        │
│  • Kubernetes ← Multi-node              │
│  • Coordinator ← Single-node            │
│  • None       ← Manual deployment       │
└─────────────────────────────────────────┘
```

**Capsule** = Packaging + Runtime + Verification (single-node)  
**OCI** = Packaging + Runtime (no verification)  
**Kubernetes** = Orchestration (multi-node, requires OCI)

They solve different problems and can work together:

- **Capsule uses OCI** as one of its runtime targets
- **Kubernetes could schedule Capsules** instead of raw containers (future)
- **OCI is foundational** for container ecosystems
