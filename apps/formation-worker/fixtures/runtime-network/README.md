# Runtime Network Phase 1 acceptance fixtures

A notes app (authored Python process route, `/health` and `/` observed,
workspace identity captured) in four variants, and one static page:

| Fixture | Route | Purpose |
|---|---|---|
| `notes` | no platform restriction | the same K and D verified on several Runtimes; the browser Contract case |
| `notes-x86-only` | `[[platform]] linux/x86_64` | an ARM64 Runtime is hard-filtered by the route's own requirement |
| `notes-arch-sensitive` | no platform restriction, but `/health` answers 503 off x86_64 | a capability match that fails K at run time; stands in for an undeclared native dependency |
| `static-page` | `ato.browser@1` serve, no build step | a route that needs no containment, so a macOS Runtime is admissible too |
| `notes-non-repeatable` | `notes` with `[effects] default = "non-repeatable"` | a route no Runtime runs unattended, whatever the request's metadata claims |

Each variant is its own Initial Condition: `source-identity` captures the
workspace, and `capsule.toml` is part of it, so the variants have different
Ks. Within one fixture, every Runtime is asked for the same K and the same D.

```sh
ato form apps/formation-worker/fixtures/runtime-network/notes \
  --runtime-network --api "$ATO_RUNTIME_NETWORK_API" --token-file <token> \
  --network dependency-resolution --mode all
```
