# pgweb — Community Desktop Import Receipt

## Test Date
2026-06-01

## Usecase
Desktop community import — launch from published community capsule.toml via `--community-toml-id`

## Result
**complete**

## Steps

1. `samples/recipes/pgweb/capsule.toml` published to production registry as `ctoml_01ksza4ny27g7g3caspr6p49qk`
2. CLI invoked:
   ```
   ATO_COMMUNITY_API_URL=https://api.ato.run \
     ato app session start github.com/sosedoff/pgweb \
     --community-toml-id ctoml_01ksza4ny27g7g3caspr6p49qk --json
   ```
3. CLI fetched TOML from `https://api.ato.run/v1/capsule-tomls/ctoml_01ksza4ny27g7g3caspr6p49qk`
4. Source identity validated via provenance (TOML has no `[source].repository`; provenance `sosedoff/pgweb` accepted)
5. OCI session started
6. Session ready: `http://127.0.0.1:41565/`
7. Session stopped cleanly

## Summary

| Field | Value |
|---|---|
| source | `github.com/sosedoff/pgweb` |
| ctoml_id | `ctoml_01ksza4ny27g7g3caspr6p49qk` |
| publish URL | `https://api.ato.run/v1/capsule-tomls/ctoml_01ksza4ny27g7g3caspr6p49qk` |
| Desktop version | — (CLI path only for this receipt) |
| commit | `feat/387-community-capsule-toml-submit` |
| OS / arch | darwin-arm64 |
| container runtime | OCI (Docker/Podman) |
| session_id | `ato-desktop-session-72580` |
| local URL | `http://127.0.0.1:41565/` |
| status | `ready` |
| stop result | `stopped: true` |

## Notes

- Desktop GUI launch path (omnibar → candidate select → `ato app session start --community-toml-id`) is wired but not GUI-exercised in this receipt (Desktop app not running in CI environment).
- CLI path (`--community-toml-id`) is verified end-to-end: fetch → source identity validation → OCI session start → ready.
- memos launch with `--community-toml-id` fails with `state 'data' requires an explicit persistent binding` — expected, persistent state setup required.
