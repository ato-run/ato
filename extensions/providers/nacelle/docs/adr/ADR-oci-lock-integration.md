---
title: "ADR-011: Integrate OCI Lock Facts into ato.lock.json"
status: draft
date: 2026-05-24
author: "@Koh0920"
related:
  - "docs/rfcs/draft/OCI_PROVIDER_LOCK_IDENTITY_SPEC.md"
  - "crates/capsule-core/src/contract/oci_compose_lock.rs"
  - "crates/capsule-core/src/contract/ato_lock/schema.rs"
  - "crates/capsule-core/src/contract/ato_lock/validate.rs"
  - "crates/capsule-core/src/engine/execution_identity/mod.rs"
---

# ADR-011: Integrate OCI Lock Facts into ato.lock.json

## 1. Context

The OCI execution path currently uses a sidecar lock file, `ato.oci.lock.json`,
to persist image digest resolutions for Compose-imported and install.sh-imported
services. This file was introduced in PR 10.6 (OCI Compose Lock Persistence) and
extended in PR 11 (install.sh intent extractor) and PR 11.5 (invariant
hardening).

It records image refs, resolved digests, platform, provider semantics, and
source/import hash. The schema is defined in
`crates/capsule-core/src/contract/oci_compose_lock.rs`:

```json
{
  "version": 1,
  "import": {
    "kind": "compose" | "docker-run-script",
    "source_path": "docker-compose.yml",
    "source_hash": "sha256:<hex>"
  },
  "images": {
    "<service-label>": {
      "declared_ref": "postgres:14",
      "resolved_digest": "sha256:<hex>",
      "platform": "linux/amd64",
      "provider_semantics": "podman-rootless-native-v1"
    }
  }
}
```

The main Ato lock, `ato.lock.json`, is defined in
`crates/capsule-core/src/contract/ato_lock/schema.rs`. It already has a flat
`ResolutionSection` with a `BTreeMap<String, Value>` that supports arbitrary
entries. The existing OCI provider spec (Section 3.4 of
`OCI_PROVIDER_LOCK_IDENTITY_SPEC.md`) prescribes a canonical location at
`resolution.oci_images.<target-label>` with `declared_ref`, `resolved_digest`,
`platform`, and `importer_input_hash`.

Long-term, maintaining two separate lock files creates ambiguity:

- **Which lock is authoritative?** When both exist, a consumer cannot determine
  which facts to trust without knowing the provenance of each.
- **How is execution identity derived?** Execution identity (V2, defined in
  `execution_identity/mod.rs`) already includes an `OciLaunchEnvelope` that
  covers image digest, platform, and provider semantics. The sidecar lock
  duplicates some of these facts outside the canonical lock identity projection.
- **How does recipe sharing work?** A shared recipe or store-submitted capsule
  needs a single lock to pin all runtime facts. Splitting OCI facts into a
  sidecar complicates recipe transport.
- **How should Desktop/store import consume the lock?** Desktop reads session
  state through CLI, not lock directly, but future recipe provenance features
  need a unified lock surface.
- **How should lock refresh behave?** Mutable tag refresh and source hash drift
  handling live in the sidecar, separate from the main lock's resolution and
  contract sections that feed `lock_id`.

Ato's state layering principle (Section 7 of AGENTS.md) states: declaration
(`capsule.toml`), resolution result (`ato.lock.json`), and live state (local
state) must not be mixed. The sidecar lock is an extra resolution-result layer
that should merge into the canonical `ato.lock.json`.

## 2. Goals

- **Make `ato.lock.json` the authoritative lock for all runtime types.** OCI
  image facts, import provenance, and provider semantics live in the main lock's
  `resolution` section.
- **Preserve OCI-specific facts.** No loss of declared ref, resolved ref,
  resolved digest, platform, provider semantics, import kind, source path, or
  source hash during migration.
- **Keep image digest and platform in execution identity.** These already
  participate in `OciLaunchEnvelope` within the V2 identity computation. The
  merged lock must feed the same fields.
- **Preserve source/import provenance.** `import.kind`, `import.source_path`,
  and `import.source_hash` remain available for lock replay and drift detection.
  These are provenance/freshness inputs, not direct execution identity inputs.
- **Allow lock refresh.** Mutable tag refresh, source hash drift detection, and
  provider semantics drift detection continue to work from the merged location.
- **Support Compose, install.sh, and native capsule recipes consistently.** All
  import paths write to the same lock structure. Multiple import sources are
  supported through a multi-entry `oci_imports` map.
- **Avoid breaking existing experimental `ato.oci.lock.json` users abruptly.**
  A phased migration preserves backward compatibility.

## 3. Non-goals

- Implement the migration in this ADR PR. This document defines the target
  model and migration strategy; implementation follows in separate PRs.
- Change OCI execution behavior immediately. The existing execution path
  continues to use `OciComposeLock` until the read path is updated.
- Solve remote registry trust completely. This ADR addresses lock structure, not
  registry verification or signed image trust.
- Add a signed recipe registry. Recipe signing is orthogonal.
- Add SBOM/provenance verification beyond current lock needs. Future work.
- Define a multi-platform image matrix. The initial model is host-specific; a
  future ADR may address cross-platform lock transport.

## 4. Current Sidecar Fields

The current `ato.oci.lock.json` (defined in `oci_compose_lock.rs:34-63`) stores:

| Field | Location | Category | Purpose |
|---|---|---|---|
| `version` | top-level | Schema | Version (always 1) |
| `import.kind` | `"compose"` or `"docker-run-script"` | Provenance/freshness | Distinguishes import source |
| `import.source_path` | file path | Provenance/freshness | Which file was imported |
| `import.source_hash` | `sha256:<hex>` of source content | Provenance/freshness | Drift detection; triggers re-resolve |
| `images.<svc>.declared_ref` | e.g. `postgres:14` | Execution identity | Image identity |
| `images.<svc>.resolved_digest` | `sha256:<hex>` | Execution identity | Pinned content address |
| `images.<svc>.platform` | `linux/amd64`, `linux/arm64/v8` | Execution identity | Platform selection |
| `images.<svc>.provider_semantics` | e.g. `podman-rootless-native-v1` | Execution identity | Coarse provider identity |

Fields explicitly absent by design (session/diagnostic only):

- `container_id` — live session state
- `network_id` — live session state
- `host_port` — allocated at runtime
- `volume_id` — live session state
- `session_id` — live session state
- `secret values` — never in lock or identity
- `timestamps` — not reproducible
- `Podman machine id` — provider diagnostic

### Three distinct identity layers

This ADR distinguishes three identity layers that must not be conflated:

**Lock replay freshness** — determines whether a cached resolution can be
reused without re-resolving:

- `import.kind`
- `import.source_path`
- `import.source_hash`
- `declared_ref` (per service)
- `selected_platform` (per service)
- `provider_semantics` (per service)
- `emulation policy` (per service)

These are provenance inputs. They answer: "was this lock produced from the same
source material?" If source_hash drifts, the lock is stale and re-resolution is
required. But the source material changing does not inherently change the
resolved launch envelope if the resulting service graph and image refs are
identical.

**Execution identity** (`execution_id`) — determines whether two launches are
functionally identical. Derived from the resolved OCI launch envelope:

- Resolved image digest (`resolved_digest`)
- Selected platform (`platform`)
- Provider semantics label (`provider_semantics`)
- Service graph shape (which services exist, their labels, `depends_on` edges)
- `run_once` lifecycle, `cmd`/`entrypoint` overrides
- Readiness probe shape and timing
- State schema and sharing policy
- Environment key closure and secret reference shape
- Network aliases and policy hashes
- Ingress route shape (when ingress lands)

Execution identity is derived from the **resolved launch envelope**, not from
importer provenance. A compose file whose comments changed but whose service
graph and image refs are identical should produce the same `execution_id`.

**Lock document identity** (`lock_id`) — the deterministic hash of the lock
document itself, computed from the canonical projection (`schema_version` +
`resolution` + `contract`, per `canonicalize.rs:10-14`). Import provenance
(`oci_imports`) is stored in `resolution` and therefore participates in
`lock_id`. This means a compose file edit that changes `source_hash` but not
the resolved launch envelope will change `lock_id` without changing
`execution_id`. This is correct: `lock_id` captures the full resolution record,
while `execution_id` captures the runtime-relevant launch envelope.

```text
source_hash change ──► lock_id changes (resolution section changed)
                  ──► execution_id may or may not change
                       (depends on whether launch envelope changed)

resolved_digest change ──► lock_id changes
                       ──► execution_id changes (launch envelope changed)
```

The existing sidecar `execution_identity_hash()` (line 96) includes
`source_hash` in its computation. In the merged model, `execution_id` is
computed from the resolved launch envelope only, not from source provenance.
This is a deliberate design change: source provenance is a freshness input, not
a functional identity input. The sidecar hash was a single combined concept;
the merged model separates it.

## 5. Proposed `ato.lock.json` Model

The main lock (`AtoLock` in `schema.rs:12-32`) uses a flat `ResolutionSection`
with `BTreeMap<String, Value>` entries. OCI image facts and import provenance
are placed under `resolution.oci_images` and `resolution.oci_imports`,
consistent with the existing `resolution.oci_images` location prescribed in
Section 3.4 of `OCI_PROVIDER_LOCK_IDENTITY_SPEC.md`.

### Target schema (within `ato.lock.json`)

```json
{
  "schema_version": 1,
  "lock_id": "blake3:<64-hex>",
  "generated_at": "2026-05-24T12:00:00Z",
  "features": {
    "declared": ["identity"]
  },
  "resolution": {
    "oci_imports": {
      "import-1": {
        "kind": "compose",
        "source_path": "docker-compose.yml",
        "source_hash": "sha256:<hex>"
      }
    },
    "oci_images": {
      "db": {
        "declared_ref": "postgres:14",
        "resolved_ref": "docker.io/library/postgres@sha256:<hex>",
        "resolved_digest": "sha256:<hex>",
        "platform": "linux/amd64",
        "provider_semantics": "podman-rootless-native-v1",
        "import_id": "import-1"
      },
      "app": {
        "declared_ref": "blinkospace/blinko:latest",
        "resolved_ref": "docker.io/blinkospace/blinko@sha256:<hex>",
        "resolved_digest": "sha256:<hex>",
        "platform": "linux/arm64",
        "provider_semantics": "podman-rootless-native-v1",
        "import_id": "import-1"
      }
    },
    "runtime": {
      "kind": "oci"
    }
  },
  "contract": {
    "delivery": {
      "install": {
        "environment": {
          "strategy": "oci_service_graph",
          "services": [
            {
              "name": "db",
              "from": "postgres:14",
              "lifecycle": "long_running",
              "depends_on": [],
              "readiness_probe": { "kind": "tcp", "container_port": 5432 }
            },
            {
              "name": "app",
              "from": "blinkospace/blinko:latest",
              "lifecycle": "long_running",
              "depends_on": ["db"],
              "readiness_probe": {
                "kind": "http",
                "container_port": 1111,
                "path": "/health"
              }
            }
          ]
        }
      }
    }
  },
  "binding": {},
  "policy": {},
  "attestations": {},
  "signatures": []
}
```

### Multi-import support

`resolution.oci_imports` is a map, not a singleton. Each entry is keyed by an
import identifier (e.g. `"import-1"`, `"import-2"`). This supports:

- Multiple compose files contributing services to one capsule
- Mixed provenance: some services from compose, some from install.sh, some from
  explicit capsule declaration
- Future Store/LockDraft import sources

Each `oci_images` entry references its import source via `import_id`. For
services declared directly in the capsule manifest (not imported), `import_id`
is absent. The literal value `"declared"` is not used in v1; absence is the
sole indicator of a non-imported target.

Example with mixed provenance:

```json
{
  "resolution": {
    "oci_imports": {
      "import-1": {
        "kind": "compose",
        "source_path": "docker-compose.yml",
        "source_hash": "sha256:<hex>"
      },
      "import-2": {
        "kind": "docker-run-script",
        "source_path": "install.sh",
        "source_hash": "sha256:<hex>"
      }
    },
    "oci_images": {
      "db": {
        "declared_ref": "postgres:14",
        "resolved_ref": "docker.io/library/postgres@sha256:<hex>",
        "resolved_digest": "sha256:<hex>",
        "platform": "linux/amd64",
        "provider_semantics": "podman-rootless-native-v1",
        "import_id": "import-1"
      },
      "cache": {
        "declared_ref": "redis:7",
        "resolved_ref": "docker.io/library/redis@sha256:<hex>",
        "resolved_digest": "sha256:<hex>",
        "platform": "linux/amd64",
        "provider_semantics": "podman-rootless-native-v1",
        "import_id": "import-2"
      },
      "app": {
        "declared_ref": "ghcr.io/acme/app:1.0",
        "resolved_ref": "ghcr.io/acme/app@sha256:<hex>",
        "resolved_digest": "sha256:<hex>",
        "platform": "linux/arm64",
        "provider_semantics": "podman-rootless-native-v1"
      }
    }
  }
}
```

In this example, `db` comes from compose, `cache` from install.sh, and `app`
from explicit capsule declaration (`import_id` absent).

### Schema version

`schema_version` remains 1 in Phase 1. The structural validator in
`validate.rs:64-68` performs a hard equality check against
`ATO_LOCK_SCHEMA_VERSION` (currently 1). Adding `oci_imports` and `oci_images`
to `resolution` does not require a schema version bump because:

1. `ResolutionSection` uses `#[serde(flatten)]` with `BTreeMap<String, Value>`,
   so unknown keys are parsed as opaque values.
2. The structural validator does not constrain which keys appear in
   `resolution.entries`.
3. Existing v1 readers that do not understand `oci_imports`/`oci_images` will
   treat them as opaque resolution data.

If a future change requires readers to understand new structural constraints
(e.g. required fields, changed enum variants), a `schema_version` bump to 2
should accompany that change, with an explicit v2-tolerant reader requirement.
The OCI lock integration does not impose such a requirement.

### Image entry fields

| Field | Required | Purpose |
|---|---|---|
| `declared_ref` | Yes | Original image ref from manifest/compose/install.sh |
| `resolved_ref` | Yes | Canonical pull reference: `<registry>/<repo>@sha256:<digest>` |
| `resolved_digest` | Yes | Content address: `sha256:<hex>` |
| `platform` | Yes | Selected platform: `<os>/<arch>[/<variant>]` |
| `provider_semantics` | Yes | Coarse provider label |
| `import_id` | No | Reference to `oci_imports` entry; absent for declared targets (never `"declared"`) |

`resolved_ref` is distinct from `resolved_digest`. The digest alone is a
content address, but materialization requires a repository context to pull from.
`resolved_ref` combines `declared_ref`'s registry/repository with the resolved
digest, producing a canonical pull reference. This improves replay clarity and
materialization stability without requiring the consumer to reconstruct
`<repo>@<digest>` from two separate fields.

### `resolved_ref` canonicalization

`resolved_ref` must be canonicalized before entering `lock_id` or
`execution_id`. Registry and repository components are normalized to their
fully-qualified form:

- `postgres:14` normalizes to `docker.io/library/postgres@sha256:<digest>`.
- `docker.io/postgres:14` normalizes to `docker.io/library/postgres@sha256:<digest>`.
- `ghcr.io/acme/app:1.0` normalizes to `ghcr.io/acme/app@sha256:<digest>`.

Equivalent declared refs that resolve to the same image must produce the same
`resolved_ref`. Canonicalization is applied at resolution time, before the lock
is written.

### `source_path` normalization

`source_path` in `oci_imports` entries is stored as a normalized project-relative
path. This ensures lock portability across hosts:

- Absolute host paths are forbidden.
- Path separators are normalized to `/`.
- Path traversal segments (`..`) are rejected or canonicalized before lock write.

Example: `/home/user/project/docker-compose.yml` is stored as
`docker-compose.yml`. `subdir\compose.yml` (Windows backslash) is stored as
`subdir/compose.yml`.

### What feeds `lock_id`

The canonical identity projection (defined in `canonicalize.rs:10-14`) includes
`schema_version`, `resolution`, and `contract`. Adding OCI image facts and
import provenance to `resolution` means they participate in `lock_id`
computation. This is correct: OCI image resolution is a resolution-time fact
that affects capsule identity. Import provenance (`oci_imports`) also
participates in `lock_id` because it is stored in `resolution`.

### Readiness probes in contract

Readiness probe shape is part of the `contract` section (which feeds `lock_id`)
and is also part of execution identity. Probes reference container ports, not
host-allocated ports:

```json
{
  "readiness_probe": {
    "kind": "http",
    "container_port": 1111,
    "path": "/health"
  }
}
```

Host-allocated ports are never written to the lock or contract. They are
session state only.

## 6. Identity Semantics

### Included in execution identity

These fields feed the V2 `OciLaunchEnvelope` in
`execution_identity/mod.rs:183`:

- Declared image ref (`declared_ref`)
- Resolved digest (`resolved_digest`)
- Resolved pull reference (`resolved_ref`)
- Selected platform (`platform`)
- Provider semantics label (`provider_semantics`)
- Emulation policy (whether QEMU/Rosetta is active for the platform)
- Service graph shape (which services exist and their labels)
- `depends_on` edges (startup ordering topology)
- `run_once` lifecycle classification
- `cmd` / `entrypoint` override shape
- Readiness probe shape and timing (container port, path, kind — never host
  port)
- State schema and sharing policy (mount shape, durability)
- Ingress route shape (when ingress lands)

### Excluded from execution identity

- Container ID
- Network ID
- Host port (allocated at runtime)
- Session ID
- Volume ID
- Generated secret values
- Timestamps
- Logs
- Live Podman machine ID
- Exact provider version string
- Diagnostic messages
- Import source hash (`source_hash`) — provenance/freshness input only

### Included in lock_id

Everything in the canonical projection (`schema_version` + `resolution` +
`contract`), including:

- `oci_images` entries (declared_ref, resolved_ref, resolved_digest, platform,
  provider_semantics, import_id)
- `oci_imports` entries (kind, source_path, source_hash)

This means `source_hash` affects `lock_id` but not `execution_id`. A compose
file edit that changes `source_hash` without changing the resolved launch
envelope will produce a different `lock_id` but the same `execution_id`.

## 7. Platform Strategy

### v1: Host-specific OCI resolution

The initial model is **host/platform-specific**. Each `oci_images` entry
contains a single `platform` and its corresponding `resolved_digest`. A lock
generated on `darwin/arm64` with `platform: "linux/arm64"` is valid for that
platform only. Running the same capsule on a `linux/amd64` host requires
relock.

```text
darwin/arm64 host ──► resolves linux/arm64 images ──► platform-specific lock
linux/amd64 host  ──► resolves linux/amd64 images ──► different lock
```

This is the correct starting point because:

1. Execution identity includes platform. A different platform is a different
   execution.
2. Multi-platform manifest resolution requires host-specific platform selection,
   which is a resolution-time decision.
3. The current sidecar model is already host-specific (one `platform` per entry).

Relock across platforms:

```sh
# On darwin/arm64, lock produced linux/arm64 images
ato lock .

# Move to linux/amd64 host, relock to get correct platform
ato lock .
```

### Future: Multi-platform matrix (not v1)

A future ADR may define a multi-platform lock where each service entry contains
a per-platform digest map:

```json
{
  "oci_images": {
    "db": {
      "declared_ref": "postgres:14",
      "resolved_ref": "docker.io/library/postgres@sha256:<index-digest>",
      "platforms": {
        "linux/amd64": { "resolved_digest": "sha256:<amd64-digest>" },
        "linux/arm64": { "resolved_digest": "sha256:<arm64-digest>" }
      },
      "provider_semantics": "podman-rootless-native-v1"
    }
  }
}
```

This would enable recipe sharing across platforms (Store/catalog use case).
However, the implementation complexity (multi-platform resolution, platform
selection at execution time, identity derivation from a platform matrix) is
significant and deferred.

## 8. Migration Strategy

### Phase 1: Dual-read, main-write, no schema bump

**Schema:** `schema_version` stays 1. `oci_imports` and `oci_images` are opaque
extensions to the existing `resolution` flat map.

**Read path:**

1. Load `ato.lock.json`. If `resolution.oci_images` is present and populated,
   use it as the authoritative source.
2. If `resolution.oci_images` is absent, fall back to loading
   `ato.oci.lock.json` via `load_from_dir()`.
3. Merged resolution is used for identity computation and lock replay.

**Write path:**

1. After OCI image resolution, write OCI image facts to `ato.lock.json` under
   `resolution.oci_images` and `resolution.oci_imports`.
2. Continue writing `ato.oci.lock.json` alongside the main lock for backward
   compatibility. Phase 2 will remove the sidecar write.
3. Each image entry gets a `resolved_ref` (canonical digest pull ref) computed
   from the declared ref and resolved digest via
   `construct_resolved_ref_from_sidecar`, and an `import_id` set to `"default"`
   for import-derived images.

**Dual-lock conflict behavior:**

When both `ato.lock.json` (with OCI entries) and `ato.oci.lock.json` exist:

| Condition | Behavior |
|---|---|
| Both exist and OCI entries are equivalent | Use main lock. No warning. |
| Both exist and entries differ | Use main lock. Emit typed warning: `oci_sidecar_lock_ignored_due_to_main_lock`. |
| Main lock has OCI entries, no sidecar | Use main lock. Normal path. |
| Main lock malformed (parse error) | Fail with typed error. Do **not** silently fall back to sidecar. |
| Main lock has no OCI entries, sidecar exists | Fall back to sidecar. Use for identity and replay. |
| Sidecar malformed, main lock has OCI entries | Ignore sidecar. Emit warning: `oci_sidecar_lock_parse_failed`. |

**Result:** New and upgraded users get OCI facts in the main lock. Existing
sidecar-only projects remain readable by new Ato via fallback. Phase 1 does
**not** guarantee downgrade compatibility: if a new Ato writes only the main
lock and the user downgrades to an older Ato, the older Ato will read a stale
sidecar. No schema version change.

### Phase 2: Main-only write, sidecar warning

**Read path:**

1. Load `ato.lock.json`. Use `resolution.oci_images` if present.
2. If `resolution.oci_images` is absent and `ato.oci.lock.json` exists, emit a
   warning suggesting migration, then use sidecar data.
3. If `resolution.oci_images` is absent and no sidecar exists, require lock.

**Write path:**

1. Write OCI facts to `ato.lock.json` only.
2. If `ato.oci.lock.json` exists, emit a warning that it is stale and can be
   removed.

**Result:** Users are nudged to migrate. No breakage.

### Phase 3: Sidecar read-only legacy import

**Read path:**

1. Load `ato.lock.json` only. `resolution.oci_images` is required for OCI
   execution.
2. If `ato.oci.lock.json` exists but `resolution.oci_images` is absent, offer
   automatic one-time migration via `ato lock migrate-oci` (or implicit
   migration on first `ato run` after upgrade).

**Write path:**

1. Write to `ato.lock.json` only.
2. Never write `ato.oci.lock.json`.

**Result:** Sidecar is fully deprecated. It may be removed in a future release
after a deprecation period.

### Migration command

```
ato lock migrate-oci
```

Reads `ato.oci.lock.json`, writes its contents into `ato.lock.json` under
`resolution.oci_images` and `resolution.oci_imports`, recomputes `lock_id`.
Does not bump `schema_version` (stays 1 in Phase 1). Alternatively, automatic
migration can occur on lock refresh if preferred.

Migration from sidecar to main lock preserves `execution_id` because the
resolved launch envelope fields are transferred identically. `lock_id` may
change because the full resolution section (including provenance) is
restructured, but `lock_id` change is acceptable during migration.

## 9. Lock Refresh Semantics

Lock refresh updates resolved digests and detects drift. The following rules
govern refresh behavior:

| Condition | Action |
|---|---|
| Mutable tag, no cached digest | Resolve via provider, write new digest |
| Mutable tag, cached digest matches source_hash + declared_ref + selected_platform + provider_semantics + emulation policy | Reuse cached digest (no provider round-trip) |
| Mutable tag, digest drift from registry | Update digest, recompute execution identity |
| Source hash drift (compose/install.sh changed) | Require re-import + re-lock |
| Platform drift | Require re-lock (changes execution identity) |
| Provider semantics drift | Require re-lock or emit diagnostic |
| Digest ref (`image@sha256:...`) | Do not churn; round-trip as-is |
| Lock refresh | Never write secret values |

These rules are consistent with the existing replay contract in
`oci_compose_lock.rs:14-17` and the `entry_is_fresh()` method (line 114).

### Identity change triggers

A lock refresh that changes any of the following alters `execution_id`:

- `resolved_digest` (new image content)
- `platform` (different architecture)
- `provider_semantics` (different provider mode)
- Service graph shape (added/removed services, changed dependencies)

A lock refresh that changes only `source_hash` (compose file edited but
resulting service graph and image refs are identical) changes `lock_id` but
does **not** change `execution_id`. Re-resolution is still triggered because
the service definitions may have changed, but if the resolved launch envelope
is identical, the execution identity is preserved.

```text
source_hash changed, launch envelope unchanged:
  lock_id: CHANGED (resolution section has new source_hash)
  execution_id: UNCHANGED (launch envelope identical)
  action: re-resolve (source may have changed service defs),
          but if resolved envelope is same, execution proceeds
          without identity disruption

resolved_digest changed:
  lock_id: CHANGED
  execution_id: CHANGED
  action: full relock required
```

## 10. Desktop/Store Implications

### Desktop

- Desktop reads session state through CLI, not lock directly for running
  sessions. The merged lock does not change Desktop's session consumption path.
- Future recipe lock provenance display (showing which images are pinned, what
  platform was selected) can read from `resolution.oci_images` in the main lock
  without parsing a separate sidecar file.
- Desktop should not need to know about `ato.oci.lock.json` after Phase 2.

### Store/registry

- Recipe submission should include lock facts from `ato.lock.json` (not from
  the sidecar). The unified lock ensures all runtime types contribute to the
  recipe's identity.
- Verified recipes should not rely on `ato.oci.lock.json` format. Store
  validation reads `ato.lock.json` only.
- Lock generation metadata (which import kind was used, source hash) is
  preserved in `resolution.oci_imports` for provenance auditing.
- For cross-platform catalog use, a multi-platform lock matrix (Section 7) may
  be needed in the future. v1 recipes are host-specific.

## 11. Compatibility Risks

| Risk | Mitigation |
|---|---|
| Existing experimental sidecar users have `ato.oci.lock.json` but no OCI entries in `ato.lock.json` | Phase 1 dual-read path falls back to sidecar |
| Duplicate/conflicting lock facts (both files exist with different data) | Main lock wins. Typed warning emitted. See dual-lock conflict behavior table in Section 8 |
| Main lock malformed | Fail with typed error. Do not silently fall back to sidecar |
| Sidecar malformed while main lock has OCI entries | Ignore sidecar with warning |
| Stale sidecar after migration to main lock | Phase 2: emit warning. Phase 3: ignore sidecar |
| Partial migration (some capsules migrated, some not) | Per-capsule migration; each capsule directory is independent |
| Changed `execution_id` after migration | Identity computation uses the same launch envelope fields regardless of source file; `execution_id` is unchanged if data is identical. Note: `source_hash` is excluded from `execution_id` in the merged model (it was included in the sidecar's `execution_identity_hash()`), so sidecar identity hash and merged `execution_id` are not directly comparable |
| `lock_id` change during migration | `lock_id` may change because the resolution section is restructured. This is acceptable during migration |
| Docs/examples referencing `ato.oci.lock.json` | Update in Phase 2/3 documentation pass |
| Host-specific lock prevents cross-platform recipe sharing | Acknowledged. v1 is host-specific. Multi-platform matrix is a future ADR |

## 12. Follow-up Implementation Plan

### PR 1 (PR #240): Lock model fields + dual-read ✅

- Add `resolution.oci_images` and `resolution.oci_imports` typed accessors to
  `AtoLock` (within the existing `BTreeMap<String, Value>` flat map).
- Add `resolved_ref` and `import_id` to image entry accessor.
- Update OCI runner read path: check `ato.lock.json` first, fall back to
  `ato.oci.lock.json`.
- Implement dual-lock conflict behavior per Section 8.
- Tests:
  - `dual_read_main_lock_wins_over_sidecar`
  - `dual_read_sidecar_fallback_preserves_execution_identity`
  - `sidecar_malformed_ignored_when_main_lock_has_oci_entries`
  - `main_lock_malformed_does_not_silently_fallback_to_sidecar`
  - `schema_v1_reader_behavior_is_explicit`
  - `cached_digest_reuse_requires_matching_platform`
  - `emulation_policy_drift_requires_relock`
  - `resolved_ref_is_canonicalized_before_identity`
  - `source_path_is_project_relative_and_normalized`

### PR 2 (PR #241): Write path in runners ✅

- Update OCI runner write path: write OCI facts to `ato.lock.json` under
  `resolution.oci_images` and `resolution.oci_imports`, while preserving
  sidecar write for backward compatibility.
- Wiring: `oci_compose_runner.rs` and `install_sh_runner.rs` both call
  `write_oci_facts_to_main_lock` after resolution, before the sidecar write.
- Each runner uses `import_id: Some("default")` with appropriate `kind`
  (`"compose"` or `"docker-run-script"`).
- Tests:
  - `compose_runner_writes_main_lock_oci_facts_alongside_sidecar`
  - `compose_runner_main_lock_source_path_is_project_relative`
  - `install_sh_runner_writes_main_lock_oci_facts_alongside_sidecar`

### PR 3: Deprecate sidecar write + update docs

- Remove sidecar write from all new flows (`--oci-compose`, `--oci-install-sh`).
- Update OCI Provider Spec, recipe docs, Desktop docs, README.
- Tests:
  - `fresh_capsule_creates_no_sidecar_lock`
  - `sidecar_stale_warning_emitted_on_dual_lock`

### PR 4: Cleanup legacy sidecar support

- Remove `load_from_dir()` fallback from runners.
- Keep `oci_compose_lock.rs` as a read-only migration utility.
- Remove `ato.oci.lock.json` from `.gitignore` suggestions and examples.
- Tests:
  - `sidecar_only_lock_fails_with_migration_hint`

## 13. Documentation Updates Needed

| Document | Update |
|---|---|
| `docs/rfcs/draft/OCI_PROVIDER_LOCK_IDENTITY_SPEC.md` | Section 3.4 already prescribes `resolution.oci_images`; update Section 13 (sidecar) to reference this ADR for migration; add `resolved_ref`, `import_id`, `oci_imports` |
| Recipe documentation | Show `ato.lock.json` as the single lock file; remove references to `ato.oci.lock.json` |
| AODD operation docs | Update lock inspection steps to read from main lock |
| Desktop docs | Note that lock provenance display reads `resolution.oci_images` from main lock |
| README | If lock behavior is user-facing, update lock file description |
| `docs/rfcs/TEMPLATE_ADR.md` | No changes needed |

## Alternatives Considered

### Option A: Keep sidecar indefinitely

- Advantage: No migration effort; existing code continues to work.
- Disadvantage: Two lock files create permanent ambiguity. Recipe sharing,
  store submission, and Desktop provenance all need to handle two sources.
  Violates "state is layered" principle (extra resolution-result layer).

### Option B: Merge into `ato.lock.json` immediately, drop sidecar

- Advantage: Clean; single source of truth from day one.
- Disadvantage: Breaks existing experimental users immediately. No backward
  compatibility period.

### Option C (chosen): Phased migration with dual-read

- Advantage: Backward compatible. Existing users are not disrupted. New users
  get the unified model immediately. Migration is gradual.
- Disadvantage: Temporary complexity in the read path during Phase 1 and Phase
  2. Requires maintaining two code paths for a limited period.
