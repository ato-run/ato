# Desky Install Spec (Draft)

**Status:** Draft v0.1  
**Updated:** 2026-04-02

## 1. Goal

Desky is the first app that should demonstrate Ato's environment materialization capability, not just native app projection.

The user-facing command is:

```bash
ato install desky
```

The desired behavior is:

- Ato resolves `desky` to a canonical scoped package id.
- `ato-cli` resolves an install graph from `ato.lock.json`.
- `ato-cli` materializes the complex local environment before first useful launch.
- The Desky Tauri app stays thin and acts as setup/chat/diagnostics UI on top of the Ato-managed environment.

This is intentionally different from a typical desktop installer where the Tauri app owns runtime installation itself.

## 2. Product Split

### 2.1 Ato responsibilities

Ato is the control plane and lifecycle manager.

- Resolve `desky` alias to canonical scoped id.
- Fetch manifest and lock data.
- Resolve runtime/tool/service artifacts from `ato.lock.json`.
- Materialize the local environment.
- Project the Desky shell as a native desktop app.
- Manage update, repair, reinstall, and uninstall.

### 2.2 Desky responsibilities

Desky is the thin user-facing shell.

- Guided onboarding.
- Workspace and project selection.
- Chat-first UI.
- Status and diagnostics UI.
- Repair entrypoint that delegates to Ato.

### 2.3 Non-goal

Desky must not become a second package manager or runtime installer.

Specifically, the Desky app should not directly own:

- Ollama installation.
- OpenCode installation.
- Lock resolution.
- Background service registration.
- OS-specific install/update logic.

## 3. Why This Fits Existing Ato

This design is aligned with the current CLI and lock model.

- `ato install` already materializes an installable object and optionally projects a launcher.
- `ato init` is already described as producing a durable `ato.lock.json` baseline.
- workspace state already distinguishes immutable lock data from mutable local adaptation.
- native projection already exists as a separate concern from install.

Therefore Desky should extend the current install pipeline rather than bypass it with app-local setup logic.

## 4. Required User Experience

### 4.1 Install

```bash
ato install desky
```

Expected high-level flow:

1. Resolve `desky` alias to `publisher/desky`.
2. Download `capsule.toml` and `ato.lock.json` for the selected release.
3. Resolve and materialize required artifacts.
4. Register the Desky native app projection.
5. Save bootstrap state for first launch.

### 4.2 First launch

On first launch, Desky should not perform heavy setup itself. Instead it should:

1. Connect to an Ato-managed local supervisor API or command surface.
2. Read the current bootstrap/materialization status.
3. Ask only for personalization inputs.
4. Request Ato to finalize environment-specific choices.
5. Open the initial chat session once health checks pass.

### 4.3 Personalization vs install

Install and personalization must stay separate.

- Install: immutable or reproducible environment graph.
- Personalization: workspace path, selected model tier, privacy mode, outside-workspace policy.

## 5. Canonical Package Identity

The current CLI contract prefers scoped ids and rejects slug-only refs for normal install. Desky should preserve that contract internally.

### 5.1 Canonical id

Desky must have a canonical scoped id:

```text
publisher/desky
```

### 5.2 Alias behavior

`ato install desky` should be supported only as a curated alias resolution path.

Rules:

- The CLI canonical install identity remains `publisher/desky`.
- `desky` is a store alias, not a new global package naming model.
- Ambiguous slug-only installs remain rejected outside the curated alias table.

This keeps the existing supply-chain posture while allowing a polished hero-app command.

## 6. Install Architecture

### 6.1 Control-plane flow

The install flow should be modeled as:

```text
ato install desky
  -> alias resolution
  -> manifest fetch
  -> lock fetch
  -> install graph resolution
  -> artifact materialization
  -> native projection
  -> bootstrap state write
```

### 6.2 Materialization phases

The install graph for Desky has four layers.

#### Layer A: UI shell

- Tauri app bundle.
- App icon and projection metadata.
- Launchable desktop shell.

#### Layer B: runtime/tool artifacts

- OpenCode engine artifact.
- Ollama runtime artifact or adapter contract.
- Any pinned helper tooling required by the environment.

#### Layer C: service graph

- Which services must exist.
- Startup order.
- Health checks.
- Repair defaults.

#### Layer D: deferred machine adaptation

- Model tier selection.
- Existing Ollama reuse vs Ato-managed install.
- Host-specific capability decisions.

Layer D must not be persisted as immutable lock truth.

## 7. Lock Model

### 7.1 Principle

For Desky, `ato.lock.json` must represent the reproducible install graph, not every machine-specific observation.

Use this split:

- `ato.lock.json` = immutable install graph.
- first-run/bootstrap state = local machine adaptation.

### 7.2 Lock content required for Desky

The lock must cover:

- Desky shell artifact.
- runtime/tool artifacts.
- service graph metadata.
- health-check contract.
- default model plan metadata.
- repair strategy metadata.

### 7.3 Lock content that must stay out

The lock should not persist host observations such as:

- actual RAM/GPU detection result.
- chosen workspace path.
- local preference toggles.
- whether the user chose Fast vs Balanced on a specific machine.

These belong in mutable local state similar to existing workspace state overlays.

## 8. Draft Manifest Shape

This section proposes the minimum new authoring surface needed for a Desky-like app.

It intentionally builds on existing concepts:

- native target for the projected shell.
- lock-driven runtimes/tools.
- external dependencies for environment pieces.

### 8.1 Draft `capsule.toml`

```toml
schema_version = "0.2"
name = "desky"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
entrypoint = "Desky.app"

[distribution]
install_strategy = "ato-managed"

[distribution.desktop]
framework = "tauri"
projection = "native"

[[targets.desktop.external_dependencies]]
alias = "opencode"
source = "ato/opencode-engine"
source_type = "store"

[[targets.desktop.external_dependencies]]
alias = "ollama"
source = "ato/ollama-runtime"
source_type = "store"

[bootstrap]
mode = "deferred-personalization"
status_source = "ato"

[bootstrap.personalization]
prompt_for = ["workspace_path", "model_tier", "privacy_mode"]

[bootstrap.models.fast]
default = "qwen3-coder:7b"

[bootstrap.models.balanced]
default = "qwen3-coder:14b"

[bootstrap.models.fallback]
provider = "external"
```

### 8.2 Notes on the draft

- `[distribution.install_strategy] = "ato-managed"` is the key declaration that the app expects Ato to materialize supporting environment pieces.
- `external_dependencies` keeps environment pieces explicit and lockable.
- `[bootstrap]` is UI-facing metadata, not immutable runtime resolution truth.

## 9. Draft Lock Shape

The current lock format already understands tools, runtimes, targets, artifacts, and dependencies. Desky needs a narrow extension rather than a parallel lock system.

### 9.1 Draft `ato.lock.json`

```json
{
  "version": "1",
  "meta": {
    "created_at": "2026-04-02T00:00:00Z",
    "manifest_hash": "blake3:..."
  },
  "capsule_dependencies": [
    {
      "name": "opencode",
      "source": "ato/opencode-engine",
      "source_type": "store",
      "resolved_version": "0.1.0"
    },
    {
      "name": "ollama",
      "source": "ato/ollama-runtime",
      "source_type": "store",
      "resolved_version": "0.1.0"
    }
  ],
  "runtimes": {
    "desktop_shell": {
      "provider": "tauri-bundle",
      "version": "0.1.0",
      "targets": {
        "aarch64-apple-darwin": {
          "url": "https://.../desky.app.tar.zst",
          "sha256": "..."
        }
      }
    }
  },
  "targets": {
    "desktop": {
      "artifacts": [
        {
          "filename": "Desky.app.tar.zst",
          "url": "https://...",
          "sha256": "...",
          "type": "desktop-shell"
        }
      ],
      "environment": {
        "strategy": "ato-managed",
        "services": [
          {
            "name": "ollama",
            "from": "dependency:ollama",
            "lifecycle": "managed",
            "healthcheck": {
              "kind": "http",
              "url": "http://127.0.0.1:11434/api/tags"
            }
          },
          {
            "name": "opencode",
            "from": "dependency:opencode",
            "lifecycle": "on-demand",
            "depends_on": ["ollama"]
          }
        ],
        "bootstrap": {
          "requires_personalization": true,
          "model_tiers": ["fast", "balanced", "fallback"]
        },
        "repair": {
          "actions": ["restart-services", "rewrite-config", "switch-model-tier"]
        }
      }
    }
  }
}
```

### 9.2 Required extension

The main addition is a target-scoped `environment` block inside the lock.

That block describes install-time and runtime materialization metadata that is:

- more structured than generic artifacts,
- still deterministic enough to lock,
- but separate from user-specific mutable state.

## 10. Local Mutable State

Desky needs a mutable local state file distinct from the canonical lock.

Suggested responsibilities:

- selected workspace path.
- selected model tier.
- whether Ollama is reused or Ato-managed.
- last bootstrap step completed.
- observed health state and repair history.

This can reuse the existing pattern already present in workspace binding and policy state.

## 11. CLI Changes Required

### 11.1 Alias resolution

Add a curated alias resolution path before the existing scoped-id rejection path.

Behavior:

- If the user runs `ato install desky`, the CLI resolves it to a configured canonical scoped id.
- If no curated alias exists, current slug-only rejection behavior remains unchanged.

### 11.2 Install execution

Add a new install branch for `install_strategy = "ato-managed"`.

That branch should:

- read the lock `environment` block,
- materialize dependencies and services,
- write bootstrap state,
- then run existing projection logic.

### 11.3 Repair command surface

Desky needs a narrow CLI or local control-plane interface such as:

- `ato app status publisher/desky`
- `ato app repair publisher/desky`
- `ato app bootstrap publisher/desky --finalize`

The exact command shape can vary, but the control plane must stay in `ato-cli`, not in the Tauri shell.

## 12. Desktop App Changes Required

Desky should reuse the existing Tauri shell pattern but remove ownership of environment installation.

The app needs only:

- bootstrap status view,
- workspace/model personalization form,
- chat session surface,
- diagnostics and repair triggers.

The app should talk to Ato-managed state rather than install runtimes directly.

## 13. Implementation Plan

### Phase 1: CLI install path

- Add curated alias resolution for `desky`.
- Keep canonical scoped-id install behavior unchanged.
- Add tests for alias hit and alias miss.

### Phase 2: lock extension

- Extend lock model with target-scoped `environment` metadata.
- Keep current `tools`, `runtimes`, `artifacts`, and `capsule_dependencies` intact.
- Add serialization/deserialization tests.

### Phase 3: materialization engine

- Add install-time environment materialization for `ato-managed` apps.
- Resolve service order and health checks from lock.
- Write bootstrap state for first launch.

### Phase 4: Desky shell

- Re-scope the Tauri app to bootstrap/chat/diagnostics only.
- Replace direct installer behavior with Ato control-plane calls.

### Phase 5: repair and lifecycle

- Add repair path in `ato-cli`.
- Ensure uninstall cleans projection, state, and managed services.
- Ensure update preserves local personalization state where compatible.

## 14. Minimal Acceptance Criteria

Desky is considered implemented at MVP level when all of the following are true.

- `ato install desky` resolves to a canonical scoped package id.
- install materializes Desky shell plus at least one supporting runtime dependency through lock resolution.
- projection registers Desky as a native app.
- first launch reads Ato bootstrap state rather than re-installing dependencies itself.
- Desky can request repair through Ato.

## 15. Explicit Non-Goals For MVP

- fully generic environment orchestration for every app category.
- machine-specific hardware adaptation persisted into canonical lock.
- all-platform parity on day one.
- Tauri updater owning release/update instead of Ato.
- Desky directly managing its own runtime installation.

## 16. Recommended First Slice

The smallest viable slice is:

1. macOS-only.
2. one curated alias: `desky`.
3. one native shell projection.
4. one managed dependency: OpenCode.
5. one optional dependency contract: existing or managed Ollama.
6. one bootstrap state file consumed by Desky UI.

That is enough to demonstrate the core claim:

**Ato installs not only an app shell, but the reproducible local environment that makes the app usable.**
