# Runtime Network Phase 1 acceptance fixtures

One notes app (authored Python process route, `/health` and `/` observed,
workspace identity captured), three routes to the same kind of K:

| Fixture | Route | Purpose |
|---|---|---|
| `notes` | no platform restriction | the same K and D verified on several Runtimes; the browser Contract case |
| `notes-x86-only` | `[[platform]] linux/x86_64` | an ARM64 Runtime is hard-filtered by the route's own requirement |
| `notes-arch-sensitive` | no platform restriction, but `/health` answers 503 off x86_64 | a capability match that fails K at run time; stands in for an undeclared native dependency |
