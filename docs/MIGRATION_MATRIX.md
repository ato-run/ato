# Computation architecture migration matrix

| Former capability | Current owner |
|---|---|
| Local/Git repository execution | `ato-adapter-repository` → `ato.workspace@1` → Nacelle provider |
| Source detection and ambiguity evidence | Repository adapter |
| Runtime/toolchain identity | Workspace residual + exact Nacelle artifact resolver |
| Dependency install and process launch | Nacelle provider |
| Build behavior | Concrete workspace transition/provider operation |
| Sandbox/filesystem/network policy | Sealed run workspace + Nacelle provider and `ato-netd`; unsupported exact allowlists fail closed |
| Secret/environment injection | Safe residual binding IDs + provider secret backend |
| Mutable session lifecycle | App-owned `Run { head }` plus PID start/boot/process-group or Job identity records |
| Snapshot build/restore | Snapshot provider registration/verification; physical restore remains provider-specific future work |
| CAS/cache persistence | `ato-objects::FsObjectStore` |
| `.capsule` import/export/signature | `ato-objects` closure bundle |
| CLI/Desktop process boundary | `ato-ipc::computation` |
| Compose/service graphs | `capsule.compose@1` semantics |
| Drift diagnosis | ComputationRef comparison + resolution/provider receipts |
| Repository resolution pin | Adapter-owned `capsule.lock`, consumed and recomputed by `ato run` |
| Heterogeneous Port payload typing | Opaque Kernel carrier + Protocol-owned validators |

Removed concepts—LockDraft, generic Program/Build/Launch/ExecutionPlan,
universal State/Connector/Record, generic InitSpec, and old artifact/wire
formats—have no compatibility facade. Their problem-solving capabilities are
owned by the concrete components above.
