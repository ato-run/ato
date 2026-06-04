# Network Notes

## Token handling
- Bearer token passed only via `Authorization: Bearer smoke` header
- Fragment `#route=/add&source=...` never contains token — token delivered separately (Desktop inserts via `ato-netd`)
- `user_visible_url` in launch response is `null` — reserved for StartServe (externally reachable); no 127.0.0.1 exposed

## Egress proxy
- `ato-netd` started automatically by `ato app session start` when `ATO_NETD_BIN` is set
- Egress proxy socket: `~/.ato/run/netd.sock`
- Ingress deregistered on stop via `ato_net::control::Client::deregister_ephemeral_ingress`

## Local runtime URL
- `local_runtime_url` in sessions response: actual host-mapped port (`http://127.0.0.1:43333/`)
- NOT the container's internal port (8080) — fixed in this smoke run
- Container responds HTTP 200 at the host-mapped URL within ~2s of launch

## Capsule source
- Source from Store CTA: `koh0920/adminer` (URL-encoded: `koh0920%2Fadminer`)
- No `publisher/slug` capsules published on `api.ato.run` in this environment
- Install profile manually pre-created for smoke (mirrors `ato install` output)
- Source validation in `POST /v1/runtime/install-profiles` accepts `publisher/slug` format
- Actual `ato install` would succeed when capsule is published
