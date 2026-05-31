# Community Desktop Import AODD Summary - 2026-06-01

## Published samples

All 6 samples published to `https://api.ato.run/v1/capsule-tomls`:

| slug | source | ctoml_id |
|---|---|---|
| memos | `usememos/memos` | `ctoml_01ksza4mjs1feg3gwvx9cy35w5` |
| uptime-kuma | `louislam/uptime-kuma` | `ctoml_01ksza4mvjtk8wthttjk3h2zxv` |
| n8n | `n8n-io/n8n` | `ctoml_01ksza4n4knkqms45eaqbp6cc7` |
| blinko | `blinkospace/blinko` | `ctoml_01ksza4ndct49z3qdsvs73p4g1` |
| excalidraw | `excalidraw/excalidraw` | `ctoml_01ksza4np2yrs1mqe7jz10ep1g` |
| pgweb | `sosedoff/pgweb` | `ctoml_01ksza4ny27g7g3caspr6p49qk` |

## Initial verification

| slug | source | ctoml_id | result |
|---|---|---|---|
| pgweb | `github.com/sosedoff/pgweb` | `ctoml_01ksza4ny27g7g3caspr6p49qk` | ready |

Desktop UI path was wired but not GUI-exercised: omnibar community candidate -> `LaunchCapsule` with `community_toml_id` -> `GuestRoute::CapsuleHandle` -> `ato app session start ... --community-toml-id ... --json`.

## Explicit state re-run

Scope: re-run the four #401 samples that were previously blocked by missing explicit persistent state binding, using `ato app session start --community-toml-id ... --attach-state ... --json`.

State directories were allocated under `.tmp/ato-aodd-state-rerun/<slug>/<state-name>` in this worktree.

| Sample | Community TOML ID | State bindings | Result |
| --- | --- | --- | --- |
| n8n | `ctoml_01ksza4n4knkqms45eaqbp6cc7` | `data` | Pass: session ready at `http://127.0.0.1:41305/`, stop clean |
| uptime-kuma | `ctoml_01ksza4mvjtk8wthttjk3h2zxv` | `data` | Pass: session ready at `http://127.0.0.1:35633/`, stop clean |
| blinko | `ctoml_01ksza4ndct49z3qdsvs73p4g1` | `db-data`, `app-data` | Narrower blocker: invalid community TOML `schema_id` format |
| memos | `ctoml_01ksza4mjs1feg3gwvx9cy35w5` | `data` | Pass: session ready at `http://127.0.0.1:38407/`, stop clean |

## Notes

- Community TOML provenance validation was exercised before each re-run launch.
- The previous `state '<name>' requires an explicit persistent binding before it can be attached` blocker was cleared for all four re-run samples.
- A missing binding run now fails early with typed `ATO_ERR_EXECUTION_CONTRACT_INVALID` and the hint `Pass: --attach-state data:/path/to/data`.
- `blinko` now stops at `Configuration error: Schema hash has invalid format: sha256:blinko-pgdata-v1`, which is a community TOML data issue rather than a session binding issue.
- Browser-tab and screenshot capture were not exercised in this CLI-only B-plan validation pass.
