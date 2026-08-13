# Pure Transforms And Lock Layers

**ステータス:** Draft  
**目的:** Ato を file-first / pure-function-first の計算モデルとして定義し、`graph lock`, `placement lock`, `session state` の責務分離を固定する。  
**前提:** Ato は single-CAS, `refs`, `gcroots`, `materialized`, `state`, `foundation profile`, `requirements / inventory / lock` を採る。

## 1. 要約

Ato では、重要な判断は hidden DB や live RPC から直接行ってはならない。  
impure な観測は許可するが、その結果は必ず snapshot file に落とし、その後の解決・評価・plan 生成は pure transform として扱う。

この仕様では、lock と state を次の 3 層に分ける。

- `graph lock`
  何を実行するか
- `placement lock`
  どこで実行するか
- `session state`
  今どう動いているか

## 2. UNIX哲学ベースの原則

### 2.1 Canonical State Is File

重要な入力・出力・判断結果は file tree として表現できなければならない。

### 2.2 Transform Before Mutation

`inputs -> outputs` の関係を先に定義し、その後に fetch / mount / launch などの副作用を行う。

### 2.3 Observe Then Snapshot

live system の観測は impure でよいが、必ず snapshot file に落としてから判断する。

### 2.4 Locks Are Derived Artifacts

lock は手続きの途中産物ではなく、入力集合から導出される artifact である。

### 2.5 DB Is Cache, Not Canonical API

DB を使ってよいが、正本 API は file tree であり、DB は高速化 index に留める。

## 3. Lock Layers

### 3.1 Graph Lock

`graph lock` は capsule graph 自体を確定する lock であり、再現性の核である。

入力:

- `capsule.toml`
- foundation requirements
- resource requirements
- trust policy
- registry/index snapshots
- optional overrides

出力:

- exact capsule / foundation / resource version
- object digests
- signature / provenance references
- normalized dependency graph

### 3.2 Placement Lock

`placement lock` は graph をどの placement candidate で実行するかを確定する lock である。

入力:

- `graph lock`
- foundation inventory snapshot
- placement snapshot
- compatibility policy
- scheduling policy

出力:

- selected placement
- selected exact foundation set
- compatibility decision
- lease prerequisites
- execution locality (`local` / `remote`)

### 3.3 Session State

`session state` は launch 後の mutable state である。

入力:

- `placement lock`
- launch request
- runtime events

出力:

- `session_id`
- process metadata
- logs
- heartbeat
- lease status

`session state` は pure である必要はない。

## 4. Canonical File Model

pure transform 系の canonical 入出力は、少なくとも次で表現できるべきである。

### 4.1 Inputs

```text
inputs/
├── capsule.toml
├── foundation.requirements.json
├── resource.requirements.json
├── trust.policy.toml
├── compatibility.policy.toml
├── foundation.profile.json
├── foundation.inventory.snapshot.json
├── placement.snapshot.json
├── registry.snapshot.json
└── overrides.json
```

### 4.2 Outputs

```text
outputs/
├── graph.lock.json
├── placement.lock.json
├── execution.plan.json
└── diagnostics.json
```

この形により、`ato resolve`, `ato lock`, `ato place`, `ato plan` は file-to-file transform として定義できる。

## 5. Pure Transform Chain

推奨する canonical chain は次の通り。

```text
manifest
  -> validate
  -> resolve dependencies
  -> graph.lock.json
  -> evaluate placement using snapshots
  -> placement.lock.json
  -> derive execution plan
  -> execution.plan.json
  -> launch
  -> session state
```

### 5.1 `manifest -> graph lock`

pure でなければならない。

### 5.2 `graph lock + snapshots -> placement lock`

snapshot を入力にすれば pure でなければならない。

### 5.3 `placement lock -> execution`

impure でよいが、出力は `session state` として file projection を持つべきである。

## 6. Pure / Impure Boundary

### 6.1 Pure にすべきもの

- manifest validation
- dependency resolution
- exact version selection
- compatibility evaluation
- placement selection
- execution plan derivation

### 6.2 Impure でよいもの

- network fetch
- current inventory observation
- GPU availability observation
- process launch
- mount / overlay / sandbox 構築
- lease heartbeat
- log streaming

### 6.3 Rule

impure な結果を pure transform に渡す場合、必ず snapshot file に落とすこと。

## 7. Snapshot Rule

### 7.1 Canonical Snapshot Files

state の canonical projection として、少なくとも次を持つ。

```text
state/
├── foundation-inventory/
│   └── current.json
├── placements/
│   └── current.json
├── trust/
│   └── current.json
└── sessions/
    └── <session-id>.json
```

DB はこれらの index としてのみ使ってよい。

### 7.2 Prohibited Pattern

次は避ける。

- hidden DB を直接読んで exact version を決める
- live RPC を直接読んで placement を決める
- manifest から直接 launch する

## 8. Compatibility Decision Function

compatibility check は pure function として定義する。

入力:

- `old_lock`
- `new_inventory_snapshot`
- `compatibility_policy`

出力:

- `compatible`
- `revalidate`
- `relock`
- `fail_closed`

## 9. CLI Meaning

このモデルにおける CLI の意味は次の通り。

- `ato resolve`
  input を正規化し、snapshot を集める前段
- `ato lock`
  `graph lock` を生成する pure transform
- `ato place`
  `placement lock` を生成する pure transform
- `ato plan`
  `execution.plan.json` を生成する pure transform
- `ato run`
  `lock -> place -> plan -> launch` の合成

## 10. Case Coverage Checklist

### A. ローカル基本ケース

1. local run, single Python version
2. local run, multiple Python versions
3. install 済み lock で offline rerun
4. resource が巨大で未取得

### B. Foundation 複数 version

5. `python=[3.10,3.11,3.11.1,3.12.2]`
6. `uv` minor update
7. `nacelle` patch update
8. exact pin
9. range only

### C. OS / driver / device 拡張

10. OS patch update
11. OS major update
12. driver patch update
13. driver major update
14. eGPU attach
15. device hotplug
16. GPU removal

### D. Placement

17. local placement possible
18. local impossible, remote possible
19. multiple remote candidates
20. candidate exists, inventory mismatch
21. inventory matches, live availability insufficient
22. availability lost during lease

### E. Preview / Promote

23. preview placement is reproducible after install
24. preview remote, promoted local
25. preview lock and install lock remain distinct

### F. State / Resource Boundary

26. model weights are resource
27. in-progress checkpoint is state
28. completed adapter promoted to resource
29. transcode cache is state/cache
30. generated artifact stored into user state

### G. GC / Retention

31. only installed root remains
32. only session root remains
33. pinned foundation survives uninstall
34. placement lock disappears after session end
35. graph lock remains while installed root exists

## 11. Invariants

- manifest から直接 session を起動してはならない
- 重要な判断は hidden DB から直接行ってはならない
- `graph lock` は pure derivation である
- `placement lock` は snapshot-based pure derivation である
- `session state` は mutable である
- canonical API は file tree である
- DB は canonical file projection から再構築できるべきである

## 12. まとめ

Ato を UNIX哲学に基づく file-first / pure-function-first system として完成させるには、CAS と state model だけでは足りない。

必要なのは、何が入力 file で、どの lock がどの pure transform の出力かを固定することである。

この仕様により、Ato は単なる stateful orchestrator ではなく、file tree を入力とする決定論的な graph derivation system として実装できる。
