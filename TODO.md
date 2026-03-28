# TODO

このファイルは、2026-03-27 時点の `main` 実装・既存 ADR・直近の設計議論を前提にした、lock-first 移行の実装方針メモです。

この TODO の主眼は次の 4 点です。

1. `ato.lock.json` を唯一の canonical input とする
2. source-started flow でも execution/build semantics を lock-shaped model に落とす
3. mutable local state と canonical reproducibility core を明確に分ける
4. desktop native-delivery を含む build/delivery を、closure-aware かつ fail-closed に再設計する

---

## Progress Roadmap

### Phase A: lock-first authority の導入

- [x] authoritative input / canonical lock precedence
- [x] lock-derived execution descriptor / runtime model
- [x] build/private publish/CI publish の主経路を lock-native 化
- [x] official publish preflight の descriptor-native 化

### Phase B: temporary manifest bridge の隔離

- [x] `CompatManifestBridge` 導入
- [x] build/publish で project root に temporary `capsule.toml` を書かない経路
- [x] injected manifest / inferred manifest fallback の in-memory bridge 化
- [x] native delivery descriptor 経路の source-location dependency 縮小

### Phase C: core 下層の compat input 整理

- [x] `CompatProjectInput` 導入
- [x] `r3_config` の workspace-root + compat input 入口
- [x] `lockfile` の compat input 入口と bridge temp write 除去
- [x] source/web packer の compat input 化
- [x] bundle/capsule packer の compat input 化
- [x] engine の compat manifest 読み取り helper 集約
- [x] `CompatManifestBridge` の public exposure を更に縮小
- [x] `r3_config::generate_config_from_parts` の compatibility wrapper を縮退または削除

### Phase D: build/publish 完了条件の固定

- [x] build/private publish/CI publish の no-materialization regression
- [x] official publish/CI publish の source-location 非依存 focused test
- [ ] build/publish helper 単位の no-materialization 証跡拡充

### Phase E: 残る manifest-path 互換面の整理

- [ ] run/install/runtime executor 側の manifest-path 互換面を別マイルストーンで切り出す
- [ ] bundle/capsule を含む compat helper の raw TOML 依存をさらに圧縮する
- [ ] `ExecutionDescriptor.manifest_path` / `manifest_dir` の run/install 限定化
- [ ] `ProducerAuthoritativeInput` の compat-only 面をさらに縮退する

### Phase F: native-delivery / closure / bootstrap の締め

- [ ] desktop native-delivery の completion coverage を validator/impl/spec で一致させる
- [ ] closure digest の publish/install/run/build surface 整合を完了させる
- [ ] bootstrap trust boundary を typed overlay / 共通 policy モデルへ収束させる

### Phase G: E2E と TODO の締め

- [ ] source-location を壊しても build/publish が動く focused regression を追加
- [ ] native-delivery / closure / bootstrap の広めの E2E を拡充
- [ ] この TODO の未完了項目を再評価して `[x]` に戻す

---

## 前提

- `ato.lock.json` を canonical input にする方針自体は採用済み
- `source inference` は実装中で、一部はまだ設計段階
- 現状は「概念の核は入ったが、closure / store / materialization / import mode の意味づけはまだ浅い」段階
- Nix 的な責務分離はかなり取り込めているが、**closure の固定・toolchain 再現性・import impurity の隔離**はまだ不十分
- desktop native-delivery は現状 README 上 `capsule.toml` 前提であり、toml レス化は未着手

---

## 0. すでに入っている土台

### 0.1 authoritative input / lock-first 境界

- [x] `ato.lock.json` を authoritative input として解決する入力分岐
- [x] canonical lock があると compatibility inputs を authority source として再利用しない fail-closed な precedence
- [x] legacy lock 単体を authoritative command-entry input として認めない挙動
- [x] single-script source-only path の基本入力分岐

### 0.2 canonical model の核

- [x] `lock_id = schema_version + resolution + contract` の canonical projection
- [x] persisted lock validation と draft/path-specific validation の基礎
- [x] unresolved marker / feature / signature placeholder の基礎モデル
- [x] `ato init` で durable な `ato.lock.json` と `.ato/*` の sidecar を materialize する流れ

### 0.3 local state / isolation / cache の下地

- [x] host isolation 用の ecosystem cache 分離
- [x] binding 系の host-side registry / local state の基礎
- [x] v3 CAS と registry store の既存ストレージ実装
- [x] tool bootstrap / runtime ensure / policy fail-closed の一部実装

### 0.4 native-delivery の現状

- [x] README 上、desktop native-delivery は `capsule.toml` canonical 前提で整理されている
- [x] current PoC は `.app` entrypoint + `codesign` finalize を中心に説明されている
- [ ] desktop native-delivery の canonical lock-first contract は未定義
- [ ] desktop native-delivery の toml レス source inference は未着手

---

## 1. 直近の最優先

## 1.1 `resolution.closure` の意味を固定する

**最優先。**
今の最大の問題は、`closure` が「観測メモ」「ecosystem lock の要約」「実行 closure identity」「build toolchain closure」のどれを指すのかがまだ揺れていること。

### やること

- [x] closure envelope / normalization / digest semantics を固定する
- [x] `resolution.closure` の最小スキーマを文書化する
- [ ] `closure` logical kinds を operational path に接続する
  - [x] `metadata_only`
  - [x] `runtime_closure`
  - [x] `build_closure`（native-delivery source-derivation path）
  - [x] `imported_artifact_closure`（compatibility import の `.app` path）
- [x] `closure_digest` の定義を固定する
- [ ] publish / install / run / build で `closure_digest` に期待してよい意味をそろえる
- [x] complete closure と incomplete/unresolved closure の区別を first-class にする
- [x] ecosystem lockfile digest を closure 全体の identity と混同しないルールを定める
- [x] desktop native-delivery で必要な `build_environment` closure を producer に接続する
  - [x] skeleton shape を array-based categories (`toolchains`, `package_managers`, `sdks`, `helper_tools`) に固定する
  - [x] toolchain
  - [x] package manager / bundler
  - [x] SDK / platform inputs
  - [x] framework CLI / helper tools

### 完了条件

- `resolution.closure` が「何を固定できていて、何をまだ固定できていないか」を型・診断・ドキュメントで同じように説明できる
- `closure_digest` を registry metadata や remote cache key に使っても誤解を生まない
- `.app` import のような impurity を pure build closure と混同しない

---

## 1.2 canonical core と local overlay の境界を固定する

### やること

- [x] `binding` / `policy` / `attestations` / `observations` の責務分離を明文化する
- [x] repo-tracked に残してよい `policy` と host-local bundle に逃がすべき `policy` を切り分ける
- [x] embedded `binding` を許すか、許すなら precedence を定義する
- [x] `sanitize_lock_for_distribution` の責務を明文化する
- [x] inspect / validate / remediation の診断文言をこの境界に合わせて統一する
- [x] `signatures` が canonical projection を対象にする標準ルールを明文化する
- [x] local derivation / projection / approval result を canonical hash 範囲外に置くことを実装・文書で統一する

### 完了条件

- `lock_id` に影響するものとしないものを実装・診断・ドキュメントで同じ説明にできる
- host-local mutable state が accidentally canonical core に混ざらない
- desktop native-delivery の `local_derivation` / `projection` が logical local overlay として位置づく

---

## 1.3 source inference の phase separation を強くする

今後の source-started flow は、曖昧な heuristics のまま execution hot path に入ってはならない。

### 段階として分けるもの

- [x] `infer`
- [x] `resolve`
- [x] `materialize`
- [x] `execute`
- [x] `import`（external artifact / impure input）
- [x] `build-derive`（pure or closure-tracked build recipe generation）

### やること

- [x] source inference ADR のうち実装済み部分と未実装部分を切り分ける
- [x] `run` の attempt-scoped materialization と `init` の workspace-scoped materialization の共有部分を抽出する
- [x] bootstrap / tool ensure / execution-plan 準備が resolve フェーズを越えて混線している箇所を洗い出す
- [x] inference は lock draft / lock-shaped model を生成するまで、build/run はその後段だけを見る、という境界をコードに落とす
- [x] unresolved state のまま実行してよいもの / だめなものをカテゴリ化する
- [x] ambiguity handling（equal-ranked candidate の扱い）を deterministic にする
- [x] execute/downstream を materialized lock-derived bridge manifest 境界へ寄せる

### 完了条件

- `run` と `init` が同じ canonical lock-shaped model を使いながら、永続化の有無だけを明確に変えられる
- source heuristics が execution semantics に残留しない
- import mode と source-derivation mode が明確に分かれる

---

## 1.4 desktop native-delivery の mode 分離を定義する

現状の README は `capsule.toml` canonical 前提。  
今後は desktop native-delivery について、少なくとも mode を分ける必要がある。

### 分けたい mode

- [x] `source-derivation`
  - source project から build closure を固定して native artifact を生成する本命モード
- [x] `source-draft`
  - inference 済みだが closure 未解決で build 不可な draft モード
- [x] `artifact-import`
  - `.app` / `.exe` / AppImage など既存ビルド成果物を imported artifact として扱うモード

### やること

- [x] `.app` / `.exe` / AppImage / `.dmg` を canonical build input と見なさないルールを明文化する
- [x] imported artifact は provenance-limited / impurity-bearing mode であることを明記する
- [x] desktop native-delivery canonical contract の top-level 論理セクションを整理する
  - [x] `contract.delivery.artifact`
  - [x] `contract.delivery.build`
  - [x] `contract.delivery.finalize`
  - [x] `contract.delivery.install`
  - [x] `contract.delivery.projection`
- [x] `process` だけでは表現できない desktop delivery semantics を contract に昇格する
- [x] source-derivation mode で何を resolve 完了と見なすか定義する
- [x] artifact-import mode で何を再現性 claim しないか明記する

### 完了条件

- [x] desktop native-delivery の “本命” と “import compatibility path” が混ざらない
- [x] README / ADR / 実装で `.app` import の位置づけが一致する
- [ ] desktop native-delivery でも lock-first canonical model が成立する

---

## 2. 次の優先度

## 2.1 ecosystem importer を first-class にする

Ato は package manager を再実装するのではなく、ecosystem truth を importer として使うべき。

### やること

- [x] `uv.lock` / `pnpm-lock.yaml` / `deno.lock` / `package-lock.json` / `Cargo.lock` / `go.sum` 等を evidence importer として整理する
- [x] 「ecosystem が解くこと」と「Ato が canonical 化すること」を分離する
- [x] `generate_uv_lock()` / `generate_pnpm_lock()` の責務を見直す
- [x] 自動生成しない方針と、将来 safe に生成してよい範囲を分けて定義する
- [x] importer provenance を inspectable にする
- [x] native-delivery framework adapter（Tauri / Electron / Wails）を importer 的に整理する

### 完了条件

- [x] Ato が package manager / framework CLI を再実装せず、ecosystem truth を importer として利用する構図が明確になる
- [x] importer の出力が canonical truth ではなく canonical 化の入力であることが明確になる

---

## 2.2 tool bootstrap の trust boundary を揃える

### やること

- [x] `ensure_uv` / `ensure_node` / `ensure_pnpm` / nacelle bootstrap / native-delivery finalize helper の扱いを比較する
- [x] download source / checksum / cache reuse / offline policy を共通モデルに寄せる
- [x] bootstrap artifact を一時 cache と durable store のどちらに置くか決める
- [x] 環境変数ベースの bootstrap policy を typed な policy overlay に寄せる方針を作る
- [x] tool bootstrap artifact を closure の一部として扱うか、host capability として扱うかをルール化する
- [x] desktop native-delivery の signing / packaging helper を toolchain closure にどう含めるか定める

### 完了条件

- [ ] runtime/tool bootstrap の安全境界を、実装ごとではなく共通ルールで説明できる
- [x] build closure と host capability の境界がぶれない

---

## 2.3 execution-plan 導出の入力境界を締める

### やること

- [x] authoritative lock がある場合に disk 上の manifest semantics を再解釈しない箇所を増やす
- [x] compatibility path のみが legacy manifest/lock を見るように整理する
- [x] lock-derived execution に必要な最小入力を型で明示する
- [x] `config.json` と execution plan が canonical input ではなく derived artifact であることをコードにも反映する
- [x] desktop native-delivery の finalize / install / projection plan も derived artifact として表現する

### 完了条件

- execution plan と `config.json` が一貫して派生物であり、authority source ではない状態になる
- finalize/project/install の host-local plan も canonical source と混線しない

注: bridge manifest への寄せは進んだが、producer/build path には temporary `capsule.toml` write を使う transitional compatibility bridge がまだ残っている

---

## 3. 中期テーマ

## 3.1 Ato native store を設計する

現状は次が別々に存在する。

- host isolation cache
- runtime/tool cache
- v3 CAS
- registry store
- local derivation / projection state
- binding / approval / observation state

必要なのは、それらをすぐ統合することではなく、まず責務を整理すること。

### やること

- [ ] store の対象を少なくとも次に分ける
  - [ ] `tools`
  - [ ] `artifacts`
  - [ ] `closures`
  - [ ] `imports`
  - [ ] `workspace-local mutable state`
- [ ] path layout 案を作る
- [ ] immutable object と mutable overlay を分離する
- [ ] GC 対象と pin 対象を設計する
- [ ] imported external artifact の格納場所と pure closure artifact の格納場所を分けるか決める
- [ ] projection / local derivation metadata の store 位置を定める

### 叩き台

- `~/.ato/store/tools/...`
- `~/.ato/store/artifacts/...`
- `~/.ato/store/closures/...`
- `~/.ato/store/imports/...`
- `workspace/.ato/...`

---

## 3.2 closure-based cache / registry key へ寄せる

### やること

- [ ] `manifest_hash` 依存が残っている箇所を棚卸しする
- [ ] `lock_id` と `closure_digest` の役割分担を整理する
- [ ] registry metadata で何を lookup key に使うべきか決める
- [ ] remote cache semantics の最小要件を定義する
- [ ] imported artifact と source-derived artifact で key semantics を分けるか検討する
- [ ] build closure / runtime closure / host materialization identity の関係を整理する

### 完了条件

- 「同じ contract/resolution なのか」
- 「同じ closure なのか」
- 「同じ imported artifact なのか」
- 「同じ host materialization なのか」

を別の識別子で説明できる

---

## 3.3 host materialization identity を導入する

`lock_id` / `closure_digest` の次に来る、host-specific realization identity の要否を判断する。

### やること

- [ ] `plan_id` 相当が必要か検討する
- [ ] target triple / runtime version / selected features / local binding / projection intent をどこまで含めるか決める
- [ ] desktop native-delivery の `local_derivation_id` / `projection_id` を host materialization identity の一部とみなすか整理する
- [ ] 実体化しないなら、その理由を ADR に残す

### 完了条件

- canonical identity / closure identity / host realization identity の三者関係を説明できる

---

## 3.4 desktop native-delivery の closure-aware 化

README の current PoC を lock-first / closure-aware に昇格するための中期テーマ。

### やること

- [ ] Tauri / Electron / Wails project を source-derivation として解決する adapter の最小仕様を作る
- [ ] framework config から build graph と toolchain evidence をどう抽出するか定義する
- [ ] desktop native-delivery の build closure に何を含めるか決める
  - [ ] Rust / Cargo
  - [ ] Node / package manager
  - [ ] Go / frontend toolchain
  - [ ] SDK / platform tools
- [ ] signing / notarization の recipe と credential の境界を分離する
- [ ] install/project/finalize のローカル state を canonical core と分離する
- [ ] imported `.app` / `.exe` の compatibility mode を別枠で文書化する

### 完了条件

- desktop native-delivery が README 上の PoC から、lock-first canonical delivery model に進化する
- “build from source” と “import prebuilt artifact” が同じ再現性 claim をしない

---

## 4. ドキュメント整備

### やること

- [ ] `current-spec.md` の manifest-first 記述と lock-first 移行状態のズレを整理する
- [ ] accepted ADR と proposed ADR の依存関係を明記する
- [ ] `closure`, `binding`, `attestations`, `policy`, `materialization`, `import`, `derivation` の用語集を作る
- [ ] review / issue / PR で使う説明文を短く統一する
- [ ] README の native-delivery 節を mode 分離に合わせて再整理する
- [ ] `.app` import は compatibility / import mode であることを README に反映する
- [ ] source-derivation mode が本命であることを明記する

---

## 5. 実装順の提案

1. `resolution.closure` と `closure_digest` の意味を固定する
2. canonical core と local overlay の境界を固定する
3. source inference の phase separation を進める
4. desktop native-delivery の mode 分離を ADR 化する
5. ecosystem importer と bootstrap trust boundary を整理する
6. Ato native store の path/layout を ADR 化する
7. registry / cache key を closure-based に寄せる
8. desktop native-delivery の source-derivation adapter に着手する
9. artifact-import mode を compatibility path として整理する

---

## 6. やらないこと

- [ ] uv / pnpm / Cargo / Go module 解決を Ato が再実装しない
- [ ] Nix language や derivation authoring をそのまま持ち込まない
- [ ] 既存の cache / CAS / store を一気に置き換えない
- [ ] `closure_digest` の意味が曖昧なまま API 依存を増やさない
- [ ] imported `.app` / `.exe` を source-derived canonical build と同じ意味で扱わない
- [ ] source heuristics を execution hot path に���し続けない

---

## 7. 判断基準

新しい実装や refactor は、次を満たすときに進める。

- [ ] authority source が 1 つに定まっている
- [ ] mutable local state が canonical hash に混ざらない
- [ ] unresolved state が silent fallback ではなく first-class marker になる
- [ ] source-started flow でも execution/build semantics は lock-shaped model に落ちている
- [ ] import mode は impurity-bearing path として明示されている
- [ ] build closure の未固定部分を「再現性がある」と称しない
- [ ] diagnostics が「何が未解決か」「何を再生成すべきか」「どこが import/host-local なのか」を fail-closed に示す
