# Dependency Derivation Cache

**Status:** Draft decision note  
**Date:** 2026-05-02  
**Scope:** per-capsule dependencies (`node_modules`, Python dependencies, provider-backed deps) の source-tree 非汚染化、derivation output cache、CAS storage layout、実行時 projection  
**Related:** [ATO_HOME_LAYOUT.md](ATO_HOME_LAYOUT.md), [HASH_AND_PROVENANCE_POLICY.md](HASH_AND_PROVENANCE_POLICY.md), [CAPSULE_FOUNDATION_RESOURCE_STATE_MEMO.md](CAPSULE_FOUNDATION_RESOURCE_STATE_MEMO.md), [BUILD_MATERIALIZATION.md](BUILD_MATERIALIZATION.md), [UNIFIED_EXECUTION_MODEL.md](UNIFIED_EXECUTION_MODEL.md)

## 1. 結論

Per-capsule dependencies は `state/` ではなく、immutable derivation output として `store/blobs/` に置く。ただし最初から package 単位や file 単位の CAS を目指さない。

Phase は次の 4 段階に分ける。

| Phase | Target | Value |
| --- | --- | --- |
| A0 | `runs/<id>/` に execution workspace を分離 | source tree 汚染ゼロ |
| A1 | derivation hash -> blob の whole-tree cache | 同一 derivation の warm run 高速化、output 検証 |
| B | pnpm / uv を managed runtime として組み込み package 単位の共有へ進む | package 単位 disk dedup |
| C | file-level CAS | 採用しない |

設計の中心は次の invariant である。

```text
store/blobs/<blob-hash>/       immutable payload only
runs/<session>/deps/           dependency projection
runs/<session>/cache/          writable session cache
state/<capsule-id>/data/       writable persistent user state
```

`node_modules` や `.venv` を user project directory、installed capsule directory、source tree に直接作らない。Phase A0 では CAS 共有をしなくてもよい。まず source tree 非汚染を達成する。

## 2. Design Decisions

### 2.1 `store/blobs/` は pure CAS にする

`store/blobs/` 配下には content-addressed payload だけを置く。`wheels/`, `site-packages/`, `node_modules-by-lockfile/` のような named layout を入れない。

Correct:

```text
store/
├── blobs/<blob-hash>/
├── refs/
│   ├── deps/<ecosystem>/<derivation-hash>.json
│   ├── installed/<capsule-id>.json
│   ├── runtimes/<name>/<version>/<platform>.json
│   ├── capsules/<capsule-id>/<version>.json
│   └── pins/<name>.json
└── meta/
    └── blobs/<blob-hash>.json
```

Wrong:

```text
store/blobs/wheels/
store/blobs/site-packages/<derivation-hash>/
store/blobs/node/<lockfile-hash>/
```

Rule:

- `blobs/` は hash で引く。
- `refs/` は名前、version、derivation、install state で引く mutable mapping である。
- `meta/` は blob の観測 metadata を保持する。

### 2.2 Blob payload と metadata を分離する

`store/blobs/<blob-hash>/` 内に `derivation-meta.json` を置かない。metadata が `blob_hash` を含むと self-reference になり、後から補正できないためである。

Payload:

```text
store/blobs/<blob-hash>/
└── node_modules/
```

Metadata:

```text
store/meta/blobs/<blob-hash>.json
```

Example metadata:

```json
{
  "schema_version": 1,
  "blob_hash": "sha256:...",
  "kind": "dependency-tree",
  "ecosystem": "npm",
  "created_at": "2026-05-02T00:00:00Z",
  "derivation_hash": "sha256:...",
  "reproducibility": "best-effort",
  "execution_strategy": "overlayfs",
  "isolation_strength": "strict"
}
```

### 2.3 Derivation hash は lockfile hash ではない

`derivation_hash` は install input identity であり、`blob_hash` は frozen output identity である。この 2 つを混同しない。

`DepDerivationKey` は JCS canonical JSON を SHA-256 で hash する。

```text
DepDerivationKey = sha256(JCS({
  ecosystem,
  package_manager,
  package_manager_version,
  runtime_name,
  runtime_version,
  platform,
  libc,
  abi,
  lockfile_digest,
  package_manifest_digest,
  package_manager_config_digest,
  install_command,
  lifecycle_script_policy,
  registry_policy,
  network_policy,
  env_allowlist_digest,
  foundation_inventory_digest,
  system_build_inputs
}))
```

Notes:

- `lockfile_digest` だけを key にしない。
- `package.json`, `.npmrc`, `pnpm-workspace.yaml`, package manager version, runtime version, platform, lifecycle policy は output に影響する。
- `native_build_inputs_digest` という名前は使わない。host-bound な header/library への依存は `system_build_inputs` として記録し、portable derivation と混同しない。

Reproducibility classification:

| Value | Meaning |
| --- | --- |
| `portable` | derivation inputs だけで output が決まる |
| `host-bound` | host headers, system libraries, CPU feature などに依存する |
| `best-effort` | lifecycle script や package manager 挙動に non-determinism が残る |

## 3. Directory Layout

最終 target layout:

```text
~/.ato/
├── store/
│   ├── blobs/<blob-hash>/
│   ├── refs/
│   │   ├── deps/<ecosystem>/<derivation-hash>.json
│   │   ├── installed/<capsule-id>.json
│   │   ├── runtimes/<name>/<version>/<platform>.json
│   │   ├── capsules/<capsule-id>/<version>.json
│   │   └── pins/<name>.json
│   └── meta/
│       └── blobs/<blob-hash>.json
│
├── state/
│   └── <slug>-<short-hash>/
│       ├── identity.json
│       ├── bindings.json
│       ├── data/
│       └── userland/
│
└── runs/
    └── <kind>-<uuidv7>/
        ├── session.json
        ├── workspace/
        │   ├── source/
        │   └── build/
        ├── deps/
        ├── cache/
        ├── tmp/
        └── log
```

`state/<capsule-id>/` は user data のみを持つ。Installed capsule の immutable refs は `store/refs/installed/<capsule-id>.json` に置く。

Example installed ref:

```json
{
  "schema_version": 1,
  "capsule_id": "byok-ai-chat-a81de4c0",
  "capsule_blob": "sha256:...",
  "dependency_blobs": ["sha256:..."],
  "runtime_blobs": ["sha256:..."],
  "installed_at": "2026-05-02T00:00:00Z",
  "version": "1.2.3"
}
```

`store/refs/deps/<ecosystem>/<derivation-hash>.json` は weak cache index であり、GC root ではない。Blob が消えていたら cache miss として再 materialize する。

Example dep ref:

```json
{
  "schema_version": 1,
  "derivation_hash": "sha256:...",
  "blob_hash": "sha256:...",
  "ecosystem": "npm",
  "created_at": "2026-05-02T00:00:00Z"
}
```

## 4. Phase A0: Source Tree Non-Pollution

Phase A0 は正しさの phase である。Disk efficiency は追わない。

### 4.1 Goals

1. User project directory と installed capsule directory に依存や build output を作らない。
2. すべての副作用を `runs/<id>/` 配下に閉じ込める。
3. Build output、dependencies、tool cache、tmp を分離する。
4. Linux, macOS, Windows CLI で動作する。

### 4.2 Workspace shape

```text
runs/<id>/workspace/
├── source/    # read-only source projection
└── build/     # writable build output
```

多くの tools は source root に `dist/`, `.next/`, `target/`, `__pycache__` を書こうとする。このため Phase A0 でも projection strategy が必要である。

Platform strategy:

| Platform | A0 strategy |
| --- | --- |
| Linux | source lowerdir + session upperdir overlay |
| macOS | copy workspace fallback |
| Windows CLI | copy workspace fallback |

Phase A0 では CAS を使わない。依存 install は session workspace 内で行ってよい。ただし source tree には書かない。

### 4.3 Invariants

```text
source tree                         read-only input
runs/<id>/workspace/source/         source projection
runs/<id>/workspace/build/          writable build output
runs/<id>/deps/                     dependency materialization
runs/<id>/cache/                    writable cache
runs/<id>/tmp/                      temporary files
```

## 5. Phase A1: Whole-Tree Derivation Output Cache

Phase A1 は A0 の上に derivation output cache を追加する。

### 5.1 Flow

1. Manifest, lock, runtime selection, platform, policy から `derivation_hash` を計算する。
2. `store/refs/deps/<ecosystem>/<derivation-hash>.json` を lookup する。
3. Ref が存在し、`blob_hash` が存在すれば CAS hit。
4. CAS miss の場合は sandbox 内 install を実行する。
5. Install output を hash し、`store/blobs/<blob-hash>/` へ atomic move する。
6. `store/meta/blobs/<blob-hash>.json` を書く。
7. `store/refs/deps/<ecosystem>/<derivation-hash>.json` を更新する。
8. `runs/<id>/deps/` に dependency projection を作る。

### 5.2 Projection strategy

Immutable store と writable runtime cache を分離する。

Linux:

```text
lowerdir = store/blobs/<blob-hash>/...
upperdir = runs/<id>/cache/<name>/
workdir  = runs/<id>/cache/.work/<name>/
mount    = runs/<id>/deps/<name>/
```

macOS:

1. Read-only compatible な package では symlink projection を試す。
2. Tool-specific cache redirect は best-effort optimization とする。
3. 書き込みが必要な可能性が高い場合、または write attempt を検出した場合は session-local copy に fallback する。

Windows:

- Phase A1 projection strategies are unsupported initially.
- Windows CLI は Phase A0 copy workspace + direct session install で動作させる。

### 5.3 Read-only is an OS property

`chmod 0555` は security boundary ではない。同一 user は `chmod` で変更できる。Read-only guarantee は mount、namespace、ACL、sandbox policy で強制する。

Linux examples:

- bind mount with `MS_RDONLY`
- mount namespace
- store owner separation for system-wide install

macOS examples:

- Seatbelt policy
- ACL / file flags where available
- fallback copy when strict read-only projection cannot be enforced

Permission bits are only hints.

## 6. Python Strategy

`.venv` 丸ごと CAS は禁止する。Python venv は relocatable ではないためである。

### 6.1 Thin session venv

```text
runs/<id>/deps/.venv/
├── pyvenv.cfg
├── bin/
│   ├── python -> store runtime python
│   └── <console scripts generated per session>
└── lib/python3.11/site-packages -> dependency projection
```

`site-packages` tree は derivation output として cache できるが、venv wrapper は session ごとに生成する。

### 6.2 Console scripts

Console scripts under `.venv/bin/` often contain absolute shebang paths. Therefore:

1. Do not reuse store-side scripts directly.
2. Generate console scripts per session.
3. Shebang must point to `runs/<id>/deps/.venv/bin/python`.
4. `pyvenv.cfg` is generated per session.

### 6.3 `.pth` relocation scan

Before freezing `site-packages`, scan `.pth` files.

Detect:

- absolute paths
- executable `.pth` lines (`import ...`)
- host path leakage

Policy:

- If safe rewrite is possible, create session-specific `.pth` output.
- If not safe, mark derivation `host-bound` or fallback to per-session install.
- Record finding in blob metadata.

### 6.4 uv integration

Phase B may use uv as managed runtime. uv's wheel cache should be integrated through refs and blobs, not by writing named directories under `store/blobs/`.

## 7. Lifecycle Scripts

Lifecycle scripts are not disabled by default. Too many real packages require them.

Policy by phase:

| Phase | Default lifecycle policy | FS write scope |
| --- | --- | --- |
| A0 | best-effort in session workspace | session workspace |
| A1 | sandboxed | install workspace tree |
| B | sandboxed package-manager strict mode | package-manager controlled scope |

User-facing modes:

| Mode | Meaning |
| --- | --- |
| default | lifecycle scripts run in sandbox |
| `--no-lifecycle-scripts` | disable lifecycle scripts explicitly |
| `--lifecycle-scripts-host` | run outside sandbox; requires explicit confirmation |

A1 sandbox constraints:

- filesystem write: install workspace tree only
- network: registry allowlist
- env: default deny
- host tools: managed runtime/toolchain only

Record result honestly:

```json
{
  "reproducibility": "best-effort",
  "lifecycle_scripts": "sandboxed",
  "network_policy": "registry-allowlist",
  "non_determinism_detected": [
    "package downloaded host-specific prebuilt binary"
  ]
}
```

## 8. Phase B: Package Manager Integration

Phase B integrates package managers rather than reimplementing them.

Responsibilities:

| Responsibility | Owner |
| --- | --- |
| derivation hash calculation | Ato |
| sandbox construction | Ato |
| `store/blobs/` placement | Ato |
| dependency resolution algorithm | pnpm / uv |
| symlink farm construction | pnpm / uv |
| lifecycle script execution | pnpm / uv inside sandbox |
| registry communication | pnpm / uv inside allowlist |

Design:

1. Ato carries pnpm and uv as managed runtimes under `store/refs/runtimes` and `store/blobs`.
2. DependencyMaterializer selects package-manager strategy.
3. pnpm virtual store and uv wheel cache are projected into the Ato store model via refs and blobs.
4. Ato does not implement npm or Python package resolution itself.

## 9. Phase C: Not Adopted

File-level CAS is intentionally not adopted.

Reasons:

1. It is too complex for Ato's target audience.
2. Stack traces and source maps become harder to read.
3. Debug workflows that inspect `node_modules` become worse.
4. Filesystem and inode pressure increase significantly.
5. Phase B gives enough disk sharing and reproducibility for the practical target.

Filesystem-level deduplication such as reflinks or ZFS dedup can be supported opportunistically, but Ato does not make file-level CAS part of the model.

## 10. DependencyMaterializer

All dependency materialization paths must go through a single brick: `DependencyMaterializer`.

It replaces ad-hoc install behavior in:

- run preflight provision command
- shadow provisioning
- provider-backed workspace materialization
- smoke preparation
- desktop dev server fallback install

Sketch:

```rust
pub struct DependencyMaterializer {
    store: StoreRef,
    sandbox: SandboxBackend,
    platform: Platform,
}

pub struct DependencyProjection {
    pub derivation_hash: Option<String>,
    pub blob_hash: Option<String>,
    pub execution_deps_path: PathBuf,
    pub env: HashMap<String, String>,
    pub cache_dirs: HashMap<String, PathBuf>,
    pub reproducibility_metadata: ReproducibilityMeta,
}

impl DependencyMaterializer {
    pub fn materialize(&self, request: DependencyMaterializationRequest) -> Result<DependencyProjection> {
        let workspace = self.create_session_workspace(&request)?;

        if !self.cas_enabled() {
            return self.install_in_workspace(workspace, request);
        }

        let derivation = self.compute_derivation(&request)?;
        match self.store.lookup_dep_blob(&derivation.hash)? {
            Some(blob) => self.project(workspace, blob, request),
            None => self.install_freeze_project(workspace, derivation, request),
        }
    }
}
```

Phase A0 uses the same brick with CAS disabled. This keeps migration incremental while making the dependency materialization path a single source of truth.

## 11. GC

GC roots:

| Root | Meaning |
| --- | --- |
| `runs/<id>/session.json` with `state == "active"` | active session dependencies stay alive |
| `store/refs/installed/<capsule-id>.json` | installed capsule strong ref |
| `store/refs/pins/<name>.json` | user/system pinned blob |

Weak references:

| Ref | Meaning |
| --- | --- |
| `store/refs/deps/<ecosystem>/<derivation-hash>.json` | weak cache mapping; not a GC root |

GC algorithm:

1. Collect blobs reachable from roots.
2. List `store/blobs/`.
3. Mark unreachable blobs as candidates.
4. Apply age and disk pressure policy.
5. Delete candidates.
6. Clean dangling refs and stale metadata.

## 12. Platform Policy

| Phase | Linux | macOS | Windows CLI |
| --- | --- | --- | --- |
| A0 | supported | supported | supported |
| A1 | supported with overlay / mount namespace | supported best-effort with symlink/copy | not initially supported |
| B | supported | supported best-effort | best-effort later |

Windows is not ignored. Phase A0 applies to Windows CLI because source-tree non-pollution is mostly path/workspace discipline. A1 projection and sandboxing are deferred because Windows requires a separate implementation strategy (junctions, symlink permissions, AppContainer, or copy-only projection).

## 13. Migration Strategy

Initial rollout:

1. Add `DependencyMaterializer` skeleton.
2. Route all install/provisioning paths through it with CAS disabled.
3. Enforce source-tree non-pollution in tests.
4. Add Phase A1 behind an opt-in flag.
5. Record metadata and compare direct install output vs derivation cache output.
6. Promote A1 to default only after provider-backed, source Node, source Python, smoke, and desktop fallback all use the same path.

Suggested flags:

```text
ATO_DEPS_WORKSPACE_ISOLATION=1
ATO_DEPS_DERIVATION_CACHE=1
```

The first flag corresponds to A0; the second corresponds to A1. Once stable, A0 becomes mandatory and A1 becomes default per platform.

## 14. Open Decisions

1. Hash algorithm label for blob paths: `sha256-...` vs `blake3-...`.
2. Exact JCS representation for `DepDerivationKey`.
3. Whether `store/meta/blobs/<blob-hash>.json` is mutable or append-only.
4. How to detect write attempts on macOS symlink projection before falling back to copy.
5. Whether `store/refs/installed` belongs in this RFC or should be moved into Home Layout proper.
6. How to represent monorepo internal packages in derivation keys.
7. Retention policy for terminated sessions that still reference dependency blobs.

## 15. Summary

The final design makes three decisions explicit.

1. Source-tree non-pollution is Phase A0 and stands on its own. It is required before CAS sharing.
2. Whole-tree dependency output cache is Phase A1 and uses derivation hash for inputs and blob hash for outputs. `lockfile-hash` is not a valid identity.
3. Package-level sharing is Phase B via pnpm / uv. Ato provides store, sandbox, projection, and policy; package managers provide ecosystem-specific resolution.

File-level CAS is not adopted. Ato should choose a runtime that is understandable, debuggable, and safe by default over a perfectly deduplicated but opaque store.