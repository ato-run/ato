# CCP — Capsule Control Protocol

**Status:** v0.5.0 (canonical)
**Schema version on the wire:** `ccp/v1`
**Owner:** ato-cli ↔ ato-desktop boundary
**Cross-references:** PAS §4.1 (Capsule Control Plane), CPDS §4.4.1 (Wire Contract)

---

## 1. Purpose

CCP is the JSON envelope that ato-desktop (host shell) uses to drive ato-cli
(provisioning + session lifecycle) as a subprocess. It is the LSP-analog for
capsule lifecycle: a stable, additive contract that lets desktop and CLI ship
on independent release trains.

Every JSON object emitted by `ato app …` and consumed by ato-desktop on stdout
is a CCP envelope.

## 2. Wire shape (top-level)

All CCP envelopes share three pinned fields:

| Field            | Type     | Value                                     |
| ---------------- | -------- | ----------------------------------------- |
| `schema_version` | string   | `"ccp/v1"` for this spec                  |
| `package_id`     | string   | `"ato/desky"` for the desktop control plane |
| `action`         | string   | one of the actions in §4                  |

The remaining fields are action-specific (see §4).

## 3. Versioning contract (additive-only within a major)

The `schema_version` field is a name-and-major pair: `"ccp/v1"` covers every
v1.x release.

**Additive-only rules for v1.x:**

1. New optional fields MAY be added at any nesting level.
2. New `action` values MAY be added.
3. New variants MAY be added to existing enum-shaped string fields, provided
   consumers ignore unknown variants.
4. **Existing fields MUST NOT** be renamed, removed, or change type.
5. **Existing field semantics MUST NOT** change.

Breaking any of (1)–(5) requires bumping to `"ccp/v2"`, which is a coordinated
ato-cli/ato-desktop release.

### Consumer tolerance

ato-desktop's CCP envelope parser MUST:

- Accept envelopes with **missing** `schema_version` (legacy CLIs predating v0.5).
- Accept envelopes with `schema_version == "ccp/v1"` as native.
- Accept envelopes with `schema_version` matching `^ccp/v[2-9]+` by **logging a
  warning and attempting best-effort parse** of the v1-shaped subset.
- Reject envelopes with malformed `schema_version` (non-string, empty, not
  matching `^ccp/v\d+$`) as a protocol error.

This tolerance lets a newer desktop drive an older CLI and vice versa within
the v0.5.x lifetime.

## 4. Message catalogue

### 4.1 `resolve_handle`

Emitted by `ato app resolve <handle>`. Tells desktop how to render a handle
(web URL, local capsule, store capsule, remote source ref).

```json
{
  "schema_version": "ccp/v1",
  "package_id": "ato/desky",
  "action": "resolve_handle",
  "resolution": {
    "input": "...",
    "normalized_handle": "...",
    "kind": "web_url | local_capsule | store_capsule | remote_source_ref",
    "render_strategy": "web | terminal | guest-webview",
    "canonical_handle": "...",
    "trust_state": { "...": "..." },
    "snapshot": { "...": "..." }
  }
}
```

Defining types: `crate::app_control::resolve::{ResolveEnvelope, HandleResolution}`.

### 4.2 `session_start`

Emitted by `ato app session start <handle>`. Returned **after** the session
process is spawned and (where applicable) reported healthy.

```json
{
  "schema_version": "ccp/v1",
  "package_id": "ato/desky",
  "action": "session_start",
  "session": {
    "session_id": "...",
    "handle": "...",
    "normalized_handle": "...",
    "canonical_handle": "...",
    "status": "running",
    "trust_state": { "...": "..." },
    "source": "...",
    "restricted": false,
    "snapshot": { "...": "..." },
    "runtime": { "...": "..." },
    "display_strategy": { "...": "..." },
    "pid": 12345,
    "log_path": "...",
    "manifest_path": "...",
    "target_label": "...",
    "notes": [],
    "guest":    { "adapter": "...", "frontend_entry": "...", "...": "..." },
    "web":      { "...": "..." },
    "terminal": { "...": "..." },
    "service":  { "...": "..." }
  }
}
```

The four optional `guest` / `web` / `terminal` / `service` discriminator
objects are mutually-exclusive in practice; exactly one is non-null per
session, matching `display_strategy`.

Defining types: `crate::app_control::session::{SessionStartEnvelope, SessionInfo}`.

### 4.3 `session_stop`

Emitted by `ato app session stop <session_id>`.

```json
{
  "schema_version": "ccp/v1",
  "package_id": "ato/desky",
  "action": "session_stop",
  "session_id": "...",
  "stopped": true
}
```

`stopped: false` is reserved for the "session was already gone" case (idempotent
stop). Errors during stop surface via the diagnostic envelope (§4.5), not via
`stopped`.

### 4.4 Bootstrap / status / repair envelopes

Emitted by `ato app install`, `ato app status`, `ato app repair`. Reference
fixtures live at `src/app_control/snapshots/{bootstrap,status,repair}.json`
and define the canonical wire shapes for these actions:

- `bootstrap_finalize`
- `status_report`
- `repair_apply`

Defining builders: `crate::app_control::{build_bootstrap_envelope,
build_status_envelope, build_repair_envelope}`.

### 4.5 E103 — missing required configuration

When the CLI cannot proceed because a required environment value is missing,
it emits a diagnostic envelope with `code: "E103"` and a `missing_schema`
field describing the field(s) the desktop UI should collect. See
`src/adapters/output/diagnostics/` for the diagnostic envelope shape; the
`missing_schema` extension was added in v0.5 to drive dynamic config UIs.

## 5. Compatibility matrix

| Desktop \\ CLI         | < v0.5 (legacy)     | v0.5.x (`ccp/v1`)  | v0.6.x+ (`ccp/v2+`) |
| ---------------------- | ------------------- | ------------------ | ------------------- |
| < v0.5 (legacy)        | works               | works (CLI emits `ccp/v1`, legacy desktop ignores `schema_version`) | undefined           |
| v0.5.x (`ccp/v1` aware) | works (no `schema_version` is treated as legacy v1) | native             | best-effort warn-and-parse |
| v0.6.x+                | undefined           | native (downgrade) | native              |

## 6. Source of truth

- Constant: `apps/ato-cli/src/app_control.rs` → `SCHEMA_VERSION`.
- Regression test: `apps/ato-cli/src/app_control/session.rs` →
  `ccp_schema_version_is_canonical_v1`.
- Snapshot fixtures: `apps/ato-cli/src/app_control/snapshots/*.json`.
- Desktop-side parser: `apps/ato-desktop/src/cli_envelope.rs` (must implement
  the §3 tolerance rules — tracked as PR-1b in `docs/v0.5-distribution-plan.md`).
