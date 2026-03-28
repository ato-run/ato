# TODO

このファイルは、2026-03-28 時点の `main` 実装・既存 ADR・直近の設計議論を前提にした、lock-first 移行の capability-first 実装方針メモです。

この TODO の主眼は、内部境界の純化それ自体ではなく、`ato run`・`ato init`・`ato publish` がより広い現実の入力に対して、fail-closed かつ再現可能に成立することです。

現時点のフェーズ進捗は次のとおりです。

- [x] Phase 0: docs / backlog alignment
- [x] Phase 1: `ato init` を desktop toml レス化の主戦場にする
- [x] Phase 2: `ato run` を lock-first consumer として締める
- [x] Phase 3: `ato publish` を lock-first consumer として締める
- [ ] Phase 4: architecture cleanup

優先順位は次の 4 点です。

1. `run` / `init` / `publish` ごとの reproducibility contract を固定する
2. source inference / import / native-delivery を含む coverage を広げる
3. desktop native-delivery の toml レス化を `ato init` 主戦場で進める
4. importer / resolver / bridge manifest / store などの構造整理を、それらを支える enablement work として後段で進める

---

## 0. 前提と現状認識

- `ato.lock.json` を canonical input にする方針自体は採用済み
- `run` / `init` は source-started でも lock-shaped model に落とす実装土台がある
- `publish` は lock-first の build / verify / upload 経路と registry metadata まで接続済みで、artifact identity / provenance の分類を保持できる
- `resolution.closure`、`closure_digest`、`contract.delivery`、unresolved marker、provenance の基礎モデルは入っている
- desktop native-delivery は mode 分離の基礎はあるが、toml レス source inference と lock-first command contract は未完了
- clean architecture 的な責務分離は重要だが、現時点では capability を支える順序で進める

### この TODO の移設ルール

- `resolution.closure`、`closure_digest`、`canonical core vs local overlay` は command contract を支える shared invariant として 2 章配下へ移す
- source inference の phase separation は coverage expansion を支える compiler/enabler として 3 章または 5 章へ移す
- native store、host materialization identity、registry rekey は 5 章へ後退させる
- desktop native-delivery は 4 章で `ato init` 中心に再記述する

---

## 1. 中心原則

### 1.1 Capability-first

- [ ] architecture の純化ではなく、`ato run`・`ato init`・`ato publish` がより多くの実アプリ入力を扱えることを最優先にする
- [ ] source inference、lock generation、execution planning、publish reproducibility を architecture cleanup より前に評価軸として固定する

### 1.2 Command-level reproducibility

- [ ] reproducibility を抽象理念ではなく `run` / `init` / `publish` ごとに定義する
- [ ] 各 command が固定すべき state と、host-local / mutable / approval-gated state を分離する
- [ ] success criteria と fail-closed 条件を command 単位で文書・診断・テストにそろえる

### 1.3 Coverage expansion before purity

- [ ] 初期段階では import / inference / framework adapter を許容する
- [ ] ただし unresolved / provenance / fallback / fail-closed は必須にする
- [ ] 暗黙 fallback と silent heuristic continuation は増やさない

### 1.4 Native-delivery as a first-class target

- [ ] desktop app を例外扱いではなく canonical lock model の対象にする
- [ ] ただし source-derived build closure と imported artifact は同じ reproducibility claim にしない
- [ ] native-delivery の評価軸を mode の美しさではなく、`run` / `init` / `publish` が成立するかで固定する

---

## 2. Command-Level Reproducibility Contract

## 2.1 Shared invariants

- [x] `ato.lock.json` が存在する場合、それを唯一の authoritative input とする
- [x] canonical lock identity は `schema_version + resolution + contract` とする
- [x] `binding` / `policy` / `attestations` / `signatures` / local derivation / projection は canonical hash から外す
- [x] `resolution.closure` は `kind` / `status` envelope を持つ normalized state とする
- [ ] `closure_digest` に期待してよい意味を `run` / `init` / `publish` でそろえる
- [ ] incomplete closure が claim してよい再現性と、claim してはいけない再現性を command ごとに固定する
- [ ] unresolved marker の reason class と blocking / non-blocking 判定を `inspect` / `validate` / diagnostics で共通化する
- [ ] fallback / import / host-local / approval-gated state を provenance と machine-readable diagnostics で共通表現にする

### 完了条件

- `closure` と `lock_id` と host-local state の関係を command 横断で同じ説明にできる
- `inspect` / `validate` / diagnostics が unresolved の内容と command への影響を同じ語彙で返せる

## 2.2 `ato run`

### 目的

source でも lock でも、execute 前には必ず immutable execution input を確定し、fail-closed に再現可能な実行へ落とす。

### 固定すべき state

- [ ] selected runtime
- [ ] process entry / executable command
- [ ] required closure materialization
- [ ] expected network / binding contract
- [ ] security gate verdict
- [ ] current attempt 用 immutable lock-derived input

### unresolved / host-local ルール

- [ ] process / runtime / closure / target compatibility / security-sensitive capability が unresolved の場合は execute に進まない
- [ ] optional metadata、説明文、非必須 hint は attempt 単位で unresolved を許容する
- [ ] actual host port allocation、secret values、host path、approval result は binding / host-local state に残す
- [ ] source-started run でも immutable input 確定後は ad hoc source heuristic を実行意味論へ持ち込まない

### desktop 対応

- [x] artifact-import run を先行実装し、`.app` / `.AppImage` / `.exe` を provenance-limited path として run 可能にする
- [x] source-derived desktop run は Tauri -> Electron -> Wails の順に追加する
- [x] imported artifact run は build reproducibility を claim しない

### 完了条件

- source-only input でも execute 前に attempt-local immutable input を確定できる
- blocking unresolved を残したまま execute に進まない
- imported artifact run と source-derived run の再現性 claim が分離される

## 2.3 `ato init`

### 目的

source input から durable baseline を固定し、後続の `run` / `publish` を ad hoc heuristic から解放する。

### 固定すべき state

- [ ] durable `ato.lock.json`
- [ ] runtime / toolchain closure の baseline
- [ ] `contract.delivery` を含む delivery build contract
- [ ] fallback / observation / importer / user-confirmed information の provenance
- [ ] explicit unresolved marker
- [ ] workspace-local `.ato/*` side state

### unresolved / host-local ルール

- [ ] ambiguity は explicit unresolved marker か user selection を要求する
- [ ] fallback 使用は lock path と provenance に必ず残す
- [ ] binding seed や approval result は workspace-local state に残し、canonical input と混ぜない
- [ ] partially resolved durable output は許容するが、deterministic re-validation を壊してはならない

### desktop toml レス化

- [x] Tauri / Electron / Wails source を read-only importer evidence から durable lock compiler 入力へ昇格する
- [x] built `.app` / `.AppImage` / `.exe` は artifact-import として lock 化し、source-derived build closure と同一視しない
- [x] `contract.delivery`、`resolution.closure`、`build_environment`、provenance、unresolved を durable に残す
- [x] signing / projection / local derivation は unresolved か host-local overlay として明示する

### 完了条件

- `ato init` が durable baseline command として成立し、desktop toml レス化の主戦場になる
- 後続 `run` / `publish` が `ato.lock.json` を主入力に進める

## 2.4 `ato publish`

### 目的

source でも artifact でも、配布可能な結果を lock-first かつ provenance 付きで再現的に出す。

### 固定すべき state

- [x] build closure
- [x] artifact identity class
- [x] publish metadata
- [x] provenance linkage
- [x] source-derived input の lock-derived build / verify / publish path

### artifact identity の分類

- [x] source-derived unsigned bundle
- [ ] locally finalized signed bundle
- [x] imported third-party artifact

### ルール

- [x] source input は lock-derived build / verify / publish path のみを通す
- [x] publish metadata は artifact identity class を保持し、同じ desktop app として混ぜない
- [x] imported artifact publish は source-derived rebuild semantics を claim しない
- [ ] registry/cache の rekey は identity class が安定してから `lock_id` / `closure_digest` へ寄せる

### 完了条件

- `publish` が source input と artifact input を lock-first に扱える
- desktop artifact identity と provenance が配布 metadata で分離される

---

## 3. Coverage Expansion Backlog

## 3.1 single-file / source-only / remote source

- [ ] single-file script input の inference / lock materialization を `run` / `init` / `publish` でそろえる
- [ ] source-only directory の compiler path を command 間で共通化する
- [ ] remote source acquisition は local/source-only と同じ compiler を通す
- [ ] remote source は初期 desktop milestone からは外すが、contract は先に固定する

## 3.2 web app / multi-service

- [ ] web app の process inference、port contract、closure materialization、publish metadata を command 単位で固定する
- [ ] multi-service の service graph / readiness / publish artifact contract を lock-first へ寄せる
- [ ] single-service と multi-service の unresolved / blocking ルールを統一する

## 3.3 ecosystem importer を first-class にする

- [x] `uv.lock` / `pnpm-lock.yaml` / `deno.lock` / `package-lock.json` / `Cargo.lock` / `go.sum` などを evidence importer として整理する
- [x] Tauri / Electron / Wails adapter を read-only importer として整理する
- [ ] importer output を `run` / `init` / `publish` の command contract へどう昇格するかを固定する
- [ ] importer provenance を inspect / validate / diagnostics から機械可読にたどれるようにする
- [ ] safe generation を許す future path と read-only observation path を分けて定義する

## 3.4 source compiler / phase separation

- [x] infer / resolve / materialize の大枠は分離済み
- [ ] `run` / `init` / `publish` が別々の heuristic 実装を持たないよう compiler 境界を固定する
- [ ] execute/downstream は materialized lock-derived input のみを見る
- [ ] compatibility import は source heuristic の別名ではなく import-side compiler handoff として扱う
- [ ] selection / confirmation / approval gate を command 契約に従って整理する

---

## 4. Desktop Native-Delivery Lock-First Roadmap

## 4.1 Supported input matrix

| Input             | `init`                      | `run`                   | `publish`                      | Notes               |
| ----------------- | --------------------------- | ----------------------- | ------------------------------ | ------------------- |
| Tauri source      | [x] durable lock 化         | [x] source-derived run  | [ ] source-derived publish     | 最優先              |
| Electron source   | [x] durable lock 化         | [x] source-derived run  | [ ] source-derived publish     | Tauri の次          |
| Wails source      | [x] durable lock 化         | [x] source-derived run  | [ ] source-derived publish     | 第三優先            |
| built `.app`      | [x] artifact-import lock 化 | [x] artifact-import run | [ ] provenance-limited publish | source build と区別 |
| built `.AppImage` | [x] artifact-import lock 化 | [x] artifact-import run | [ ] provenance-limited publish | source build と区別 |
| built `.exe`      | [x] artifact-import lock 化 | [x] artifact-import run | [ ] provenance-limited publish | source build と区別 |

## 4.2 Phase A: `init` first

- [x] desktop app の source tree から durable `ato.lock.json` を生成できるようにする
- [x] `contract.delivery`、`resolution.closure`、`build_environment`、provenance、unresolved を durable baseline に残す
- [x] build contract と host-local finalize / projection / install state を分ける
- [x] fallback や不足 evidence を lock と provenance に残す

## 4.3 Phase B: `run` / `publish` を lock-first consumer にする

- [x] `run` は lock-derived execution input だけで進める
- [x] `publish` は lock-derived build / verify / publish metadata だけで進める
- [x] compatibility manifest は import input / transitional bridge に後退させる
- [x] source-derived build と artifact-import が同じ再現性 claim をしないことをテストで固定する

## 4.4 Phase C: 後段の cleanup

- [x] temporary `capsule.toml` write を compatibility-only path へ押し戻す
- [x] bridge manifest の責務を縮小し、derived artifact として位置づける
- [ ] planner / installer / finalize / projection の責務を lock-first contract に合わせて厳密化する
      build/publish と native-delivery build path はかなり整理できたが、run/install は transitional manifest path / dir をまだ保持する

### run/install に残す transitional surface

- [ ] `run` の process/session 記録は `manifest_path` をまだ保持する
- [ ] `run` の shadow workspace / preview session は `shadow_manifest_path` と `manifest_dir` ベースの互換面をまだ使う
- [ ] `install` は preview/manual recovery と projection persistence のため `source_manifest_path` をまだ保持する
- [ ] これらの surface は build/publish の semantic authority とは切り離したまま、run/install 専用の移行対象として後段で縮退する

---

## 5. Enabling Architecture Cleanup

## 5.1 importer / resolver / bridge manifest / derived IR

- [ ] importer は canonical truth ではなく compiler input であることをコード境界でも徹底する
- [ ] resolver / planner / bridge manifest / derived IR の責務を command contract 後追いで厳密化する
- [x] temporary compatibility bridge を compatibility path に閉じ込める

## 5.2 canonical core vs local overlay

- [x] `binding` / `policy` / `attestations` / `observations` の責務分離を進める
- [ ] host-local mutable state が canonical projection に混ざらないことを sanitize / publish / inspect で共通化する
- [ ] local derivation / projection / approval result を overlay として統一表現にする

## 5.3 native store / identity / cache rekey

- [ ] Ato native store の責務を `tools` / `artifacts` / `closures` / `imports` / `workspace-local mutable state` に分ける
- [ ] immutable object と mutable overlay を path layout で分離する
- [ ] `manifest_hash` 依存を棚卸しし、`lock_id` / `closure_digest` / imported artifact identity / host materialization identity の役割分担を固定する
- [ ] registry metadata / remote cache がどの identity を使うべきかを定義する
- [ ] host materialization identity が必要なら、その入力と用途を明文化する

## 5.4 bootstrap trust boundary

- [x] `ensure_uv` / `ensure_node` / `ensure_pnpm` / nacelle bootstrap / native finalize helper の分類は整理済み
- [ ] bootstrap artifact、host capability、network bootstrap の扱いを command contract から見て同じ語彙で説明できるようにする
- [ ] desktop native-delivery の signing / packaging helper を build closure claim と host capability claim で混同しない

---

## 6. Docs / Backlog Alignment

- [x] `TODO.md`、`docs/current-spec.md`、source inference ADR、implementation tickets を同じ原則と同じ用語にそろえる
- [x] `run` / `init` / `publish` の contract table を `current-spec` に追加する
- [x] native-delivery の supported input matrix を `current-spec` に追加する
- [x] source inference ADR を「shared infer/resolve/materialize engine」中心から「3 command を成立させる shared compiler」中心へ言い換える
- [x] `inspect` / `validate` が返す machine-readable category を backlog と spec で固定する
- [ ] README / README_JA の native-delivery 節を後続で同じ方針に寄せる

---

## 7. Test / Acceptance Checklist

- [x] `ato init` が Tauri / Electron / Wails source から durable `ato.lock.json` を生成し、`contract.delivery.mode`、closure 状態、provenance、unresolved を残す
- [x] `ato init` が `.app` / `.AppImage` / `.exe` を artifact-import として lock 化し、source-derived build closure を主張しない
- [x] `ato run` が source-only input から attempt-local lock を作り、entry/runtime/closure/security/network contract が未解決なら execute に進まない
- [x] `ato run` が imported artifact を provenance-limited path で実行でき、build reproducibility を claim しない
- [ ] `ato publish` が source-derived unsigned / locally finalized signed / imported artifact を別 identity として扱い、metadata と provenance を分離する
      現状は source-derived unsigned / imported artifact を分離済みで、locally finalized signed の first-class producer path は後続
- [x] authoritative `ato.lock.json` がある場合、`capsule.toml` や ad hoc file scan が実行意味論を上書きしない
- [ ] `inspect` / `validate` / diagnostics が unresolved、fallback、host-local、import path、blocking / non-blocking を明示する

---

## 8. やらないこと

- [ ] uv / pnpm / Cargo / Go module 解決を Ato が再実装しない
- [ ] Nix language や derivation authoring をそのまま導入しない
- [ ] cache / CAS / store を一気に置き換えない
- [ ] `closure_digest` の意味が曖昧なまま API 依存を増やさない
- [ ] imported `.app` / `.AppImage` / `.exe` を source-derived canonical build と同じ意味で扱わない
- [ ] source heuristic を immutable input 確定後の execution / publish semantics に残し続けない
