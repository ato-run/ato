# Computation architecture migration matrix

| Former capability | Current owner |
|---|---|
| Local/Git repository execution | `ato-adapter-repository` → `ato.workspace@1` → Nacelle provider |
| Source detection and ambiguity evidence | Repository adapter |
| Runtime/toolchain identity | Workspace residual |
| Dependency install and process launch | Nacelle provider |
| Build behavior | Concrete workspace transition/provider operation |
| Sandbox/filesystem/network policy | Nacelle provider and `ato-netd` service |
| Secret/environment injection | Safe residual binding IDs + provider secret backend |
| Mutable session lifecycle | App-owned `Run { head }` cursor records |
| Snapshot build/restore | Snapshot provider materialization |
| CAS/cache persistence | `ato-objects::FsObjectStore` |
| `.capsule` import/export/signature | `ato-objects` closure bundle |
| CLI/Desktop process boundary | `ato-ipc::computation` |
| Compose/service graphs | `capsule.compose@1` semantics |
| Drift diagnosis | ComputationRef comparison + resolution/provider receipts |

Removed concepts—LockDraft, generic Program/Build/Launch/ExecutionPlan,
universal State/Connector/Record, generic InitSpec, and old artifact/wire
formats—have no compatibility facade. Their problem-solving capabilities are
owned by the concrete components above.
