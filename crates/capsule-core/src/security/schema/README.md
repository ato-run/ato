# capsule-core / schema

Canonical JSON Schema sources for ato capsules. These are the definitions that
external consumers (the registry web-api, `SKILL.md`, future `ato encap` /
`ato validate` lints) must agree on.

## Modules

- `capabilities` — the four capability descriptors (`Network`, `FsWrites`,
  `SideEffects`, plus a `secrets_required` bool), grouped under
  `Capabilities`. Attached to `CapsuleRequirements` as an optional
  `capabilities` block.

## Exported JSON Schema

The authoritative JSON Schema lives at
`apps/ato-cli/core/schema/capabilities.schema.json` and is regenerated from
`capabilities.rs` by:

```bash
cargo run -p capsule-core --bin export_capabilities_schema \
    > apps/ato-cli/core/schema/capabilities.schema.json
```

CI must check this file is in sync with the Rust source — any change to the
Rust enums without a matching regeneration is a build failure.

## Reconciliation with `health.toml` `network_mode`

`samples/tools/schemas/health.schema.json` defines a `requires.network_mode`
enum that predates this module. The two vocabularies map as follows:

| `health.toml` `network_mode`        | `capabilities.network` | Notes                                           |
| ----------------------------------- | ---------------------- | ----------------------------------------------- |
| `none`                              | `none`                 | Exact.                                          |
| `allowlist` + non-empty `allow_domains` | `egress`           | Domain list is not preserved in capabilities.   |
| `allowlist` + empty `allow_domains` | `none`                 | Empty allowlist ≡ no egress.                    |
| `offline-ok`                        | `egress`               | Lossy: `offline-ok` means "works without network but may use it if present". `ato validate` must emit a lint warning on this coercion and recommend declaring the stricter `none` when the capsule truly needs no network. |

Reverse direction (capabilities → `network_mode`) is not supported; the
health schema is richer and must be filled in manually.

## Schema versioning

`SCHEMA_VERSION` in `capabilities.rs` is pinned at `"1"`. Bump on any
breaking change to a variant name or on removal of a field. Clients (web-api
prompt, `SKILL.md` frontmatter) carry the same version and will refuse to
run against an unknown version.

Additive changes (new enum variants, new optional fields) do not require a
version bump, but should be noted in `CHANGELOG.md`.
