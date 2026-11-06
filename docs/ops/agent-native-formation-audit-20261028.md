# Ato Agent-Native Formation 監査（2026-10-28）

状態: docs-only 監査。実装変更なし。CI と #1573 自動パッチ生成は対象外。
基準: ato origin/main f55c4b34e / ato-api origin/main 247e905e (stg c81faba6) / ato-pwa origin/main 867f73bd。
本監査は Production / 共有 Staging のデータを書き換えていない（読み取りとローカル実行のみ）。

## 0. 結論

**足りないのは Formation Engine ではなく、「リモート MCP から既存の Formation / Verification / 公開機能へ入る入口」と「K/D 証拠の可視化」である。**

- リモート管理 MCP（https://api.ato.run/v1/mcp）は **39 tools** を持ち、GitHub URL からの source-only intake、static Web の Schema 形成、Instance 作成、データ/Operation まで実装済み。ローカルインストール不要（OAuth のみ）。
- ただし **非 static（process / OCI / portable）は operator 承認が必須**で、MCP に承認ツールが無い。**ZIP を発行する MCP tool が無い**。**publish / unpublish は MCP からも delegated token の REST 直叩きからも不可**（delegated_credential_not_allowed）。
- K/D 探索エンジン（FrozenSearch / Candidate / Proposal / Attempt / Receipt）は **main に存在**するが、入口は CLI（ato form / form-session / form-verify / runtime-network）とローカル stdio MCP（ato-formation-session-mcp）だけ。リモート MCP には K 候補・attempt・receipt を読む tool が無い。
- PWA の「Studio」は未接続（/studio は Creator analytics、CapsuleStudioPage はテストのみ）。/formations/:id/input は画面だけあり API が無い（Detected mismatch）。
- したがって推奨は、**既存 service の薄いラッパーを 4〜6 tools 追加**し、Skill は手順・判断基準だけを持つこと。新しい Agent Framework / Formation Engine は不要。

## 1. 監査方法と判定

- ato の作業ツリーは nightly のため、Rust/CLI の主張は必ず origin/main の blob で確認した（git show origin/main:path）。ato-api も origin/main 基準。
- 判定: 実装済み / 部分実装（flag OFF・operator 承認・ローカル限定・UI 未接続）/ 未実装 / UNVERIFIED。
- 「実装済み」はコードの存在と入口の存在を確認したもの。staging/production の実配置・実動作は UNVERIFIED（deploy 禁止）。

## 2. 横断監査

### 2.1 サーフェス

| サーフェス | 実体 | 主要コード | 状態 |
|---|---|---|---|
| CLI | ato 0.8.1 | apps/ato/apps/cli/src/lib.rs | init/resume/stop/encap/pack/run/reform/model-set/app/export-plan/export/login/upload/form/form-inspect/form-input/form-session/form-verify/runtime-network/version |
| Remote management MCP | /v1/mcp（ato-app-management） | apps/ato-api/src/routes/management_mcp.ts、mount src/index.ts | **39 tools**、OAuth、stateless JSON-RPC |
| Remote activity MCP | /v1/controller/mcp（ato-coop-activity） | apps/ato-api/src/routes/activity_mcp.ts | 10 tools、Controller bearer、96 KiB |
| Local activity MCP | ato-activity-mcp（stdio） | apps/ato/apps/cli/src/activity_mcp.rs、bin/ato-activity-mcp.rs | 9 tools、Actor connection file 必須（ローカル） |
| Local formation session MCP | ato-formation-session-mcp（stdio） | apps/ato/apps/cli/src/formation_session_mcp.rs、bin/ | 3 tools（status/next/submit）、relay broker（ローカル） |
| PWA（人間向け） | Add App / App 詳細 / Discover / Settings Permissions | apps/ato-pwa/src/components/personal-apps/* | 実稼働 GUI。source ZIP/folder、.capsule import、Update、Discover publish、MCP 権限 |
| PWA Studio | CapsuleStudioPage | apps/ato-pwa/src/components/studio/CapsuleStudioPage.tsx | **未接続**（import 元はテストのみ。/studio は CreatorPage） |
| Formation Search / Runtime Network | CLI + formation-worker | apps/ato/lib/formation/src/search.rs、apps/ato/apps/formation-worker/src/runtime_network/* | main に実装。CLI/ローカル MCP のみ |
| Hosted formation worker | ato-formation-worker serve | apps/ato/apps/formation-worker/src/bin/ato-formation-worker.rs | staging は formation-sugamo（apps/ato-api/docs/ops/release-jsx-storage-plaza-2026-09-12.md） |

### 2.2 MCP にある機能と実装元（管理 MCP 39 tools）

- Instance ランタイム系 16: read/mutate data port、access request、observe/list operations、invoke/get operation、assets 5種、list/get data。実装は /v1/instance-controllers/*（instance_controller.ts）。instance_data / operation / assets は feature flag で隠れる（feature_policy.ts）。
- Source / Formation 系 14: create_source_archive、get_source_archive_admission、create_app_schema、create_app_schema_version、get/wait_formation_job、get_app_schema、source 一覧/検索/窓読み/patch preview/patch 形成、upload_icon。実装は compute_sources.ts / formation_wait.ts。
- アカウント / Discover / Instance 管理 9: get_account_identity、search_apps、fork_app、add_app、create_instance、list/get/update/delete_instance。
- in-process dispatch: invoke_instance_operation と get_account_identity は HTTP を経由しない（メモリ上限・secret 経路対策）。
- 冪等性: JSON-RPC の Idempotency-Key は削除され、tool 引数 idempotency_key のみが REST のキーになる。同キー異入力は 409。

### 2.3 CLI にしか無い機能

- ローカル実行・debug: ato run / ato app（start/stop/inspect） / ato init/resume/stop / encap / pack / export / export-plan / model-set。
- Formation Search / K-D 探索: ato form / form-inspect / form-input / form-session / form-verify / runtime-network serve / reform。
- portable .capsule の生成・object graph 移送: ato pack / encap / export / upload。
→ これらは「self-host / local / debug / advanced」という CLI の役割として許容。問題は、クラウド agent が同じ検証・探索をリモートで行えないこと。

### 2.4 PWA にしか無い機能

- Discover 公開 / 非公開（DiscoverPublishPanel → POST /v1/discover/apps、/:id/unpublish）。
- 表示メタデータ編集（PATCH /v1/studio/capsules/:id/metadata、PATCH /v1/computes/:computeId/metadata）。
- Run の開始 / 停止 / consent、Instance の権限設定・MCP 権限の確定、presence、Creator analytics。
- 逆に MCP にしか無いもの: GitHub source-only intake（create_source_archive）、source の cursor/digest-fenced 編集、fork_app、upload_icon（ChatGPT file param）。

### 2.5 Formation / Verification / 登録の重複

| 領域 | 経路 A | 経路 B | 関係 |
|---|---|---|---|
| GitHub 解決 | github_repo_resolve.ts（shared） | github_public.ts / authoring_submissions.ts | 共有 service。MCP create_source_archive も同じ resolver |
| source → Schema | compute_sources/service.ts → formation worker | authoring_sessions / submission_wizard（Studio / Store 世代） | **別系統**。後者は flag OFF、Store 公開向け |
| K/D 探索 | ato_formation search / proposal（Rust、CLI） | API は K を計算しない（manifest を worker に渡すだけ） | **CLI と API で実装が別** |
| portable bundle | capsule_bundles.ts（validation / runtime verification / import） | PWA portableApplicationClient | 共有。MCP 未露出 |
| Instance | compute_instances（現行） | /v1/runs（legacy） | 現行は Instance |
| publish | discover_apps（現行） | store_apply（旧 Store） | 別 product line |

**Detected mismatch**: API の source lane は K/D の receipt を作らない（kd 監査）。CLI の form/verify は receipt を作る。この差が「MCP で作った Instance に検証証拠が無い」原因。

### 2.6 API 呼び出し関係（要約）

- Agent → MCP /v1/mcp → 内部 Hono（instanceControllerRoutes / computeInstanceRoutes / formationWaitRoutes / computeSourceRoutes / discoverRoutes）→ D1 / R2 / KV。
- Formation job → D1 formation_jobs → formation-sugamo の ato-formation-worker が /v1/internal/formation/claim で取得、sandbox build、/results で schema/materialization を返す。
- Instance 起動 → runtime launch spec → runner lease → hosted runner（ubuntu-sugamo slot1-6）または connected runner → sandbox → app proxy。
- Activity MCP → /v1/controller/mcp → runtime_activities routes → Activity room / Runner controller。
- Formation Session MCP → local session bridge → formation-worker runtime_network session（typed proposal）→ Runtime attempt。

### 2.7 認証・権限・成果物の受け渡し

- 管理 MCP: OAuth 2.1 + PKCE S256 + DCR。access 15分 / refresh 30日 / connection 90日。scope は apps:read/write/delete + instance_access:request。consent は PWA /oauth/authorize。
- Instance 権限: OAuth scope とは別に account×Instance の共有パーミッション（no/read/write/full）で data/operation を判定。scope instance_data:read / instance_operations:execute は同意画面と token に載るが、サーバー強制点が見当たらない（UNVERIFIED、現状は安全側）。
- 成果物: source は D1/R2 の archive、Formation は static materialization + schema、portable は bundle + validation + runtime verification receipt。Instance は app_url を返すが、MCP は readiness / receipt を返さない。
- Capsule と Application の同一性: K = contract_ref（BoundContract の内容アドレス）、D = derivation_ref。Discover listing は schema を指す別 row、Instance は所有者ごと。公開範囲は listing の source_visibility と公開状態で分離。

## 3. MCP 機能マトリクス

### 3.1 管理 MCP（リモート、39 tools）

| 分類 | 実装済み | 部分実装 | 未実装 |
|---|---|---|---|
| GitHub source intake | create_source_archive（public、source-only） | requested_outputs が static 以外は operator 承認待ち | private repo / ZIP upload tool |
| Schema 形成 | create_app_schema / version / patch | process / OCI / portable は承認時のみ | D 選択、K 候補比較 |
| Formation 監視 | get/wait_formation_job | 失敗コードは返るが attempt/receipt 一覧は無い | evidence tool |
| Instance | create/list/get/update/delete | 停止 / reset / readiness は無い | — |
| データ / Operation / Asset | 16 tools（flag 依存） | instance scope の強制が不明瞭 | — |
| 公開 | — | — | publish / unpublish（**delegated token では REST 直叩きも 403**） |
| メタデータ | — | REST PATCH は apps:write で可 | MCP tool 無し |
| Capsule Bundle | — | — | prepare / upload / import（REST は session auth のみ） |
| Local 実行 | — | — | CLI のみ（設計上許容） |

### 3.2 Local MCP

| MCP | tools | 前提 | リモート agent から可? |
|---|---|---|---|
| ato-activity-mcp | 9（HTTP facade は 10。get_operation を省き invoke 内で poll） | Actor connection file（0600）、ローカルプロセス | No |
| ato-formation-session-mcp | 3（status / next / submit） | owner 発行の scoped connection または relay。owner token を agent に渡さない | No（stdio / relay のみ） |

### 3.3 判定

- エージェントが「作る・試す・直す」のうち、**作る（static）と Instance 化は可**、**試す（検証証拠）・直す（K/D 探索）・公開する**は不可。
- CLI なしで完結する範囲は「小〜中規模の静的 repo / HTML・JS を GitHub URL から static Web として形成し、private Instance で使う」まで。TS/Python/OCI は operator 承認、publish は PWA。

## 4. 呼び出し経路図

### 4.1 管理 MCP（現在）

~~~mermaid
sequenceDiagram
  participant U as User (ChatGPT/Claude)
  participant C as MCP Connector
  participant M as ato-api /v1/mcp
  participant S as Services
  participant D as D1/R2
  participant F as ato-formation-worker (formation-sugamo)
  participant R as Hosted Runner
  participant P as PWA

  U->>C: repo URL を private app に
  C->>M: OAuth (DCR/PKCE) + tools/call
  M->>S: create_source_archive (GitHub source-only)
  S->>D: source job (pinned commit)
  M->>S: create_app_schema (static_web)
  S->>D: formation_jobs(queued)
  F->>D: /v1/internal/formation/claim
  F->>F: sandbox build
  F->>D: results (schema + static materialization)
  M->>S: wait_formation_job -> ready
  M->>S: create_instance
  S->>D: compute_instances (app_url)
  U->>P: My Apps で app_url 確認
  Note over U,P: publish は MCP 不可。PWA の明示操作のみ
~~~

### 4.2 K/D 探索（現在はローカル）

~~~mermaid
sequenceDiagram
  participant O as Owner CLI
  participant W as formation-worker Runtime Network
  participant B as ato-formation-session-mcp (stdio)
  participant A as Agent (Claude Code 等)
  participant R as Runtime

  O->>W: ato form --runtime-network (search, budget, deadline)
  W->>W: FrozenSearchV1 (K frozen)
  O->>B: --connection <scoped file>
  A->>B: status / next / submit(output_json)
  B->>W: session request (typed)
  W->>W: Proposal validation / CandidateRegistry / budget
  W->>R: attempt claim + execute + verify
  R->>W: observations / attempt result
  O->>W: ato form-verify --authorize-functional-verification
  W->>W: Source Result / receipt
~~~

### 4.3 レイヤ

| Layer | 実装 | agent 入口 |
|---|---|---|
| Agent-facing | 管理 MCP / local stdio MCP | OAuth MCP / local process |
| API | ato-api Workers | REST + JSON-RPC |
| Formation | ato-formation-worker（hosted） | internal formation API（token） |
| Verification | ato_formation search / scenario verifier / receipt | CLI form-verify / receipts |
| Runtime | Runner lease / sandbox | runner token |
| Product | PWA / Discover | 人間 GUI |

## 5. CLI 依存一覧

| # | 能力 | 現行の入口 | MCP 代替 | 影響 |
|---|---|---|---|---|
| 1 | ローカル source の formation（build / sandbox） | ato form / form-inspect | 無し | クラウド agent からローカル FS 不可（設計上も送信は同意必要） |
| 2 | K 候補の探索・比較・確定 | ato form --runtime-network / form-session / form-verify | 無し（local session MCP は CLI 同梱） | 任意 repo の K/D 成立を agent が回せない |
| 3 | D の明示選択・再検証 | ato form --route/--exact-runtime/--search-id、ato reform | 無し | 2 番目の D を試せない |
| 4 | portable .capsule の生成・移送 | ato pack / encap / export / upload | 無し（MCP に bundle upload 無し） | CLI 無しでは portable 経路を使えない |
| 5 | 検証 receipt / attempt 履歴 | CLI 出力 / formation-worker | 無し（job 成否のみ） | 「動く証拠」を agent が示せない |
| 6 | 明示 publish | PWA | 無し（REST も 403） | agent が公開まで完結できない |
| 7 | メタデータのみ更新 | PWA / REST PATCH | 無し（REST は可） | MCP tool 追加で解消可能 |
| 8 | ローカル実行・デバッグ | ato run / app | 無し | CLI の役割（許容） |
| 9 | Runtime 提供 | ato runtime-network serve | 無し（hosted runner はある） | self-host は CLI の役割（許容） |

区別: 「CLI を内部で使う」ことと「ユーザーに CLI を要求する」ことは別。1〜3 はクラウド側入口の欠落、4〜7 は API/service はあるが MCP 露出が無い。

## 6. K/D 探索の状態管理レビュー

### 6.1 K（Contract / Identity）

- 権威は Rust lib/formation。ContractRef = sha256(JCS(BoundContract))、DerivationRef = sha256(JCS(BoundDerivation))（authoring.rs:611,617）。K は route/source/host/時刻を含まない。
- 入口は capsule.toml（capsule_toml.rs:54）と preset（preset.rs:586）の 2 フロントエンドで、bind()（authoring.rs:665）に合流。API は K を計算しない（manifest を worker に渡すだけ、service.ts:452,616）。
- 複数 K 候補は preset 列として生成されるが、CLI は最初の成功で打ち切る（local.rs:216-223）。比較 UI / API は無い。
- 明示受諾は PWA の verify-session → confirm が唯一（authoring_submissions.ts:1662,1728、confirm_publish.ts:935）。CLI/MCP に受諾 tool は無い。
- K は内容アドレスで不変。reform / bind_derivation_against_contract は K を書き換えない（authoring.rs:683）。

### 6.2 D（Derivation / Realization）

- D は preset / capsule.toml / generation から draft → bound → candidate（SearchCandidate, search.rs:95）→ admitted（placement hard-filter）→ issued（attempt）。
- 決定論的コア decide_next（search.rs:722）+ allowed_choices（decision.rs:316）。provider は列挙から選ぶだけ。CLI の run は第一位候補を暗黙選択。
- PWA の D 選択は dev/staging 限定。delivery-mode UI は API 実装が無い死んだ並行契約（kd 監査）。
- attempt は 3 層: SearchStateV1.attempts（search.rs:147-210）、AttemptJournal（runtime-attempt/src/journal.rs:199、UNKNOWN を残す）、D1 per-attempt receipt（drizzle/0267）。
- receipt は contract_ref + derivation_ref + attempt_id の 3 点一致で束縛（receipt.rs:64-101）。hosted でも再検査（capsule_bundles.ts:2136-2147）。target は cli-local / ato-run-hosted / formation-runtime（verify.rs:198-211）。
- Scenario Verifier は ato.contract.scenario@1。Formation では deferred → fail-closed（verify.rs:52-55）。
- 環境適用範囲: receipt の target / environment / runtime が判定材料。K 変更時は contract_ref が変わるため receipt は束縛しない。

### 6.3 状態機械（要約）

| 対象 | 状態 | Owner | 証拠 |
|---|---|---|---|
| K | draft → bound → frozen → satisfied(deferred) → fully_satisfied → submitted → confirmed → published | Rust / owner / API | contract_ref、receipt、candidate status |
| D | drafted → bound → candidate → admitted → issued → claimed → pass/fail/unknown → selected/verified | Rust / Runtime | derivation_ref、attempt_id、receipt |
| Search | open → proposal rounds → attempts → termination（completed/exhausted/cancelled/deadline） | owner | FrozenSearch / SearchState / budget |

### 6.4 agent 反復の不足（4 点）

1. K 候補の一覧・比較が無い（CLI は最初の成功のみ、API は candidate 1 行）。
2. 構造化失敗コードを MCP から読めない（job_outcome は code/stage/attempt_id を返すが、MCP は生 JSON を返すだけ。探索の failure_code は API に出ない）。
3. 明示 D 選択 tool が無い（preset 自動選択のみ）。
4. attempt history tool が無い（Rust / D1 には材料があるが公開面が無い）。

## 7. CLI なし 10 ステップ評価

目標: 「この GitHub Repo を ato.run で動かして。まずは自分だけで使いたい」

| # | 手順 | 現状 | 根拠 |
|---|---|---|---|
| 1 | Repo 取得・解析 | **可（public、static 目的）**。create_source_archive が server-side で pinned commit を取得 | management_mcp.ts:313-320 |
| 2 | K 候補の形成 | **不可（remote）** / 可（CLI form-session） | Proposal / FrozenSearch は main にあるが remote tool 無し |
| 3 | K の確定 | **不可（remote）** / 可（CLI form-verify） | owner token + 明示 authorize |
| 4 | D 候補の導出 | **部分可**（preset 自動）。明示選択は不可 | requested_outputs は static/process/oci/portable を明示できるが非 static は承認待ち |
| 5 | 実行・検証・失敗時再試行 | **部分可**。job 失敗コードは構造化。attempt/receipt は不可 | job_outcome.ts / formation_wait.ts |
| 6 | ato.run 環境での Verified D | **部分可（static）**。process/OCI は operator 承認。receipt は MCP 非公開 | compute_sources/service.ts:689-756 |
| 7 | Capsule Bundle 登録 | **不可（MCP）**。REST は session auth のみ | capsule_bundles.ts:507-520 |
| 8 | Private Application 作成 | **可** | create_instance |
| 9 | PWA からの実行 | **可**（既存 E2E） | 前回リリース検証 |
| 10 | 明示 Publish | **不可（MCP）** / 可（PWA） | discover_apps.ts:1499,1571。delegated token は 403 |

結論: CLI なしで通るのは「public repo → static Web → private Instance → PWA で実行」まで。K/D の探索・検証・公開は PWA か CLI に戻る必要がある。

## 8. 公開 / 非公開 / ローカル 3 ケース

| ケース | 使えるもの | 不足 |
|---|---|---|
| 公開 GitHub Repo | create_source_archive / create_app_schema(github, static_web) / wait / create_instance | 非 static は承認待ち。K/D 探索・receipt・publish は不可 |
| 非公開 GitHub Repo | GitHub App installation の authed routes（github_public.ts:458-540）、publisher の installation token | MCP tool 無し。ユーザーが GitHub App を install している必要。agent に token を渡さない設計は維持 |
| ローカル source | ato form（ローカル sandbox）、CLI formation search、PWA folder/ZIP upload | クラウド agent からローカル FS 不可。MCP に local bridge 無し。送信は明示同意が必要 |

方針: ローカル source / secrets を暗黙に外部送信しない。MCP は bytes を受け取らない（ZIP は別 REST、icon のみ file param）。リモート MCP に ZIP/Repo 取得を足す場合も、ユーザーが添付したファイル / 明示 URL を入力にする。

## 9. セキュリティ・運用

- OAuth: DCR、PKCE S256、resource 完全一致、authorize 10 分、code 60 秒 1 回、access 15 分 / refresh 30 日 / connection 90 日、refresh 再利用で connection 失効。consent は PWA /oauth/authorize。CSRF は HMAC。
- 権限: OAuth scope（apps read/write/delete + instance_access:request）と account×Instance 共有パーミッションの二層。instance_data:read / instance_operations:execute は同意画面にあるがサーバー強制点が不明（安全側）。delete_instance_asset が apps:read で destructive（設計意図は UNVERIFIED）。
- Secrets: launch spec は SecretGrantV1（name + grant_ref）のみで値を持たない。Runner が lease で redeem。Formation 変数は owner が form-input で供給し、this_formation / reusable（expiry）を選択。artifact 埋め込みは明示時のみ。MCP からの secret 供給経路は無い（安全側だが、agent 単独完結には足りない）。
- EffectPolicy: runtime_network/select.ts の SAFE_EFFECTS で effect を除外、bindings は Phase 1 拒否、browser verifier は bwrap containment 必須。
- 隔離: formation worker は bwrap / Landlock、network は job policy に従う。containment 不能なら serve しない。Runner は lease の launch spec で実行し、Docker socket / 管理 credential / privileged を渡さない。
- 上限: source 512 files / 4 MiB per file / 16 MiB total / 8 MiB zip。Formation attempt fence / idempotency / job digest 重複拒否。Search は deadline（最大 7 日）、transfer bytes、attempts / proposal / inspection budget。中断は owner の cancel / deadline。
- K 変更・receipt 失効: K は frozen、receipt は 3 点一致。portable import は canonical bytes / digest / closure / declared route を実行前に検証し fail closed。
- 公開制御: 管理 MCP に publish 無し。PWA publish は icon 必須 + same-origin + owner 認証。自動公開経路は無い。
- 残リスク: MCP の create_instance は app_url を返すが receipt / readiness を返さない。ユーザーが「動く」と誤認する余地。

## 10. 受入シナリオ A〜F

| シナリオ | 判定 | 根拠 |
|---|---|---|
| A. 公開 GitHub Repo → 非公開アプリ | **PARTIAL** | public static は create_source_archive → create_app_schema(static_web) → create_instance で可。K/D 探索・receipt は不可。TS/Python は要承認 |
| B. D 初回失敗を agent が修正 | **PARTIAL / UNVERIFIED** | job_outcome は code/stage/attempt_id を返す。source patch で再形成可能。attempt history / receipt / D 選択 tool は無い |
| C. K 固定で別 D を検証 | **PARTIAL / UNVERIFIED** | CLI reform / portable v3 の複数 D は main。remote MCP は preset 自動選択のみ |
| D. 登録済みアプリを PWA で実行 | **PASS** | 前回リリース検証（静的/サーバー型/永続化 + UI 受入） |
| E. ローカル source から Formation | **PARTIAL（CLI 必須）** | ato form はある。remote MCP / クラウド agent からは不可 |
| F. 不正権限・不一致 K・無効 receipt を拒否 | **PARTIAL** | OAuth / Instance 権限 / bundle validation は実装済み。MCP に receipt を提示する経路が無く、agent フローで実証できない |

## 11. 重複と統合点

| 領域 | 実装 A | 実装 B | 統合 |
|---|---|---|---|
| GitHub 解決 | github_repo_resolve.ts | github_public.ts / authoring_submissions.ts / MCP create_source_archive | 共有 resolver を MCP から使う（既に create_source_archive が使用） |
| source → Schema | compute_sources/service.ts → formation worker | authoring_sessions（Studio / Store 世代、flag OFF） | 現行は前者。後者は退役判断 |
| K/D 探索 | ato_formation search / proposal（CLI） | API は K を計算しない | CLI/ローカルを advanced とし、remote には read-only evidence を追加 |
| portable bundle | capsule_bundles.ts | PWA portableApplicationClient | MCP に upload/import tool を追加 |
| publish | discover_apps（現行） | store_apply（旧） | Discover に寄せる |
| 表示メタデータ | compute metadata PATCH | studio capsules PATCH | 現行の Instance 系に寄せる |

## 12. Skill の必要機能と既存実装の再利用

### 12.1 既存 Skill

| Skill | 依存 | 現行 CLI との差 | 再利用 |
|---|---|---|---|
| ato-capsule-builder | capsule.toml 0.3、ato run -U .、ato deploy | -U / deploy は無い。Formation は preset 方式 | 退役。health / egress / 127.0.0.1 の知見のみ移植 |
| ato-store-publish | ato validate / lock / run . | validate / lock は現行に無い | 退役。checklist を新 skill へ |
| store-apply-community | 本番 D1 直書き、ssh、gh、Discord | agent に渡せない | 退役。operator runbook |
| cross-runner-verify | /v1/runs、consent/stop、旧 receipt、ato runner | runner コマンドは無い。run/consent/stop は REST で有効 | ロジックのみ移植 |
| ato-staging-capsule-lifecycle-e2e | Studio UI（未接続） | 対象画面が無い | 保留 |
| ato-desktop-mcp-flow | ato-desktop-mcp（MCP のみ） | 現行 | as-is 再利用（MCP-only の参照実装） |
| formation 専用 skill | — | **存在しない** | 新規必要 |

### 12.2 新 Skill（3 本）

1. **ato-agent-app-form**: repo を読み、static で足りるか判断し、create_source_archive / create_app_schema(github, static_web) → wait_formation_job → （失敗コード別の修正）→ create_instance。禁止: 勝手な publish、secret 出力、他者 Instance。
2. **ato-agent-verify-publish**: job_outcome / receipt の読み方、publish の前提（icon、title、license/provenance の明示確認）、publish 後の確認。
3. **ato-agent-runtime-ops**（任意）: Instance data / operation / asset の権限、consent、停止。既存 MCP 権限 UI と組み合わせる。

### 12.3 Skill が呼ぶ実在 tools（現状）

search_apps → create_source_archive → create_app_schema → wait_formation_job → get_app_schema → create_instance → （PWA で確認）→ PWA で publish。
不足は §13 の P0/P1 で補う。

## 13. P0 / P1 / P2

### P0（CLI なしの目標フローに必須）

| ID | 不足 | 再利用元 | 最小の追加 |
|---|---|---|---|
| P0-1 | 非 static（process / OCI / portable）が operator 承認のみ | compute_sources/service.ts の admission、formation worker、capsule_bundles | MCP に「承認要求 + 状態参照」tool を追加（承認自体は人間/operator のまま）。agent が blocked 理由を理解できるようにする |
| P0-2 | ZIP upload / bundle import の MCP tool が無い | source-uploads/zip、capsule_bundles prepare/content/complete/import | OpenAI file param 型の upload tool + bundle import tool（REST は既存） |
| P0-3 | publish / unpublish が MCP 不可（delegated token も 403） | discover_apps publish/unpublish | MCP tool を追加し、delegated token を許可する専用認可（scope apps:write + 明示 confirm + audit）を design。勝手に公開しない |
| P0-4 | 検証 evidence（attempt / receipt / readiness）が MCP に無い | formation_attempts / formation_results / capsule bundle receipt / instance readiness | read-only tool 3 種 |
| P0-5 | PWA FormationInputPage の API 不整合 | — | API を実装するかページを撤去（dead path を残さない） |

### P1

- P1-1: K 候補の比較・確定を remote で行う（Formation Search の owner API + 3 tools の remote 化。新 scheduler は作らない）。
- P1-2: D の明示選択・複数 D 試行を remote で行う。
- P1-3: private GitHub repo の MCP 経路（GitHub App installation、owner 同意）。
- P1-4: メタデータのみ更新 tool（REST PATCH は apps:write で可）。
- P1-5: PWA Studio の routing 判断（使うなら接続、使わないなら削除）。delivery-mode の死んだ契約も整理。
- P1-6: Skill の新規 3 本 + 旧 skill 退役。

### P2

- MCP tool の説明・error code の統一（scope 不足が HTTP 403 と JSON-RPC error の 2 形態、引数エラーも 2 形態）。
- publish 前のプレビュー（何が公開されるか）。
- 費用 / budget の agent 向け表示。
- ローカル source 送信の明示同意フロー。
- Skill の durable handoff（receipt 保存・再開）。

## 14. 最小差分の実装計画

### Phase 1（P0、既存 service の薄いラッパー）

1. **approval status tools**: get_source_archive_admission は既にある。process / application の承認状態を返す tool と、blocked 理由の構造化。
2. **upload tools**: ZIP（OpenAI file param）と .capsule bundle の prepare/complete/import。既存 REST を呼ぶだけ。
3. **publish / unpublish tools**: discover service を呼ぶ。icon 必須、confirm 引数、audit event。delegated token の専用認可を design（scope apps:write + confirm）。
4. **evidence tools**: get_formation_attempts / get_verification_receipt / get_instance_readiness。既存 tables を read-only。
5. PWA FormationInput の不整合修正（実装 or 撤去）。

### Phase 2（P1）

6. Formation Search の remote 化（status / next / submit / cancel）。既存 FrozenSearch / ProposalBatch / budget をそのまま使う。
7. private GitHub / metadata 更新。
8. PWA Studio routing と delivery-mode 契約の整理。

### Phase 3（P2）

9. Skill 3 本 + 旧 skill 退役。
10. docs: MCP tool reference、publish の明示性、失敗コード、ChatGPT/Claude 手順の更新（chatgpt-management-mcp.md のツール表は 19 tools のまま）。

**やらないこと**: 新しい Agent Framework / Formation Engine / D scheduler。既存 ato_formation + formation-worker + ato-api service の組み合わせで足りる。

## 15. CLI 不要 E2E の受入条件

前提: ato.run アカウント、ChatGPT/Claude コネクタが https://api.ato.run/v1/mcp に OAuth 接続済み。CLI 未インストール、ローカル process なし。

手順:
1. ユーザーが公開 GitHub repo URL を貼り、「まずは自分だけで使いたい」と指示。
2. agent は create_source_archive で pinned commit を取得し、create_app_schema(source=github, requested_outputs=["static_web"]) を実行。
3. wait_formation_job で ready を待ち、job の compute_schema_id を取得（最新 schema で代用しない）。
4. （新）get_formation_attempts / get_verification_receipt で、対象環境で K を満たした evidence を確認。満たさない場合は Instance を作らない。
5. create_instance で private Instance を作り app_url を返す。
6. ユーザーは PWA の My Apps から app_url を開き、200 と期待内容を確認。
7. Discover に公開されていないことを確認（明示 publish 前）。
8. ユーザーが「公開して」と明示した時だけ（新）publish_app が動く。icon/title/description/公開範囲を確認し audit に残す。

Pass 条件:
- CLI / ローカル runtime / ローカル FS なしで 1〜6 が完了。
- K/D identity（contract_ref / derivation_ref / digest）と target / environment が receipt に記録。
- 失敗系: build 失敗・権限不足・不一致 K・無効 receipt のいずれかで Instance が作られない / publish されない。
- 再試行: 同一 idempotency key で二重作成しない。budget / deadline 超過で停止。
- secrets が tool 出力 / log / receipt に出ない。

現在の阻害: P0-3（publish tool / delegated 認可）、P0-4（evidence tool）、P1-1/2（K/D 探索 remote）、P0-1（非 static 承認）。static のみなら 1〜6 のうち 1〜3,5〜7 は既存 tool で可能。receipt と publish が無いため Pass 条件は満たさない。

## 16. 最終回答

「ユーザーが CLI をインストールせず、普段使っている AI エージェントから、任意の Repo を ato.run で使えるようにするには何が足りないか」

1. **検証証拠の公開**: MCP で作った Instance には K/D の receipt / attempt が無い。既存の formation_attempts / formation_results / receipt を read-only tool で見せ、Instance 作成前に「対象環境で K を満たしたか」を agent が判断できるようにする。
2. **非 static の経路**: process / OCI / portable は operator 承認が必須。承認は人間のままでよいが、agent が blocked 理由を理解し、承認後に再開できる tool / 状態が必要。
3. **アップロード経路**: ZIP / .capsule を MCP から送れない。OpenAI file param 型の upload tool が必要（ChatGPT は MCP 経由で file を渡せる。upload_icon が実例）。
4. **明示 publish の agent 化**: 既存 Discover publish を MCP tool にし、delegated token の専用認可と明示 confirm を design。自動公開はしない。
5. **K/D 探索の remote 化**: Formation Search の 3 tools を remote に。K 候補の比較・D 選択・attempt 履歴を agent が使えるようにする（新 engine 不要）。
6. **Skill**: formation 専用 skill が無い。repo 読み → static 判定 → 失敗コード別修正 → 検証 → private → 明示 publish の手順と判断基準を持つ薄い skill を作る。旧 skill（validate/lock/runner serve）は現行 CLI に無く、退役が必要。
7. **PWA Studio の整理**: CapsuleStudioPage は未接続、/formations/:id/input は API 無し。PWA を「MCP と同じ基盤の人間 GUI」にするなら Detected mismatch を解消する。

推奨アーキテクチャ: 管理 MCP を唯一の agent-facing surface とし、既存 REST service への薄い tool を 4〜6 個追加。Formation Search / Runtime Network / local MCP は self-host・debug・高度な探索用として残す。PWA は同じ REST を使う人間 GUI として維持。新しい Agent Framework / Formation Engine は不要。

## 付録 A. 参照

- 管理 MCP: apps/ato-api/src/routes/management_mcp.ts（39 tools、JSON-RPC、proxy、冪等性）、mount src/index.ts
- OAuth: apps/ato-api/src/routes/management_oauth.ts、src/services/management_oauth/{core,grants}.ts、PWA ManagementOAuthConsent.tsx / StandaloneRoutes.tsx
- Activity MCP: apps/ato-api/src/routes/activity_mcp.ts、apps/ato/apps/cli/src/activity_mcp.rs、docs/recipes/ACTIVITY_MCP.md
- Formation session MCP: apps/ato/apps/cli/src/formation_session_mcp.rs、formation_session_mcp/relay.rs、bin/ato-formation-session-mcp.rs
- Formation Search: apps/ato/lib/formation/src/search.rs、proposal.rs、authoring.rs、verify.rs、receipt.rs、apps/ato/lib/runtime-attempt/src/journal.rs
- Owner verify/input: apps/ato/apps/cli/src/formation_verify.rs、formation_input.rs、lib.rs
- source intake: apps/ato-api/src/services/compute_sources/{input,service,source_archive}.ts、routes/compute_sources.ts
- formation job: apps/ato-api/src/services/formation/{registry,job_outcome}.ts、routes/formation_wait.ts、routes/internal_formation.ts
- Discover publish: apps/ato-api/src/routes/discover_apps.ts
- Capsule bundles: apps/ato-api/src/routes/capsule_bundles.ts
- GitHub resolve / private: apps/ato-api/src/services/github_repo_resolve.ts、routes/github_public.ts
- Authoring / Studio（flag OFF）: routes/authoring_submissions.ts、authoring_sessions.ts、studio_capsules.ts、apps/ato-api/wrangler.toml の SUBMISSION_WIZARD_ENABLED
- PWA: apps/ato-pwa/src/app/routes/AccountRoutes.tsx、components/studio/CapsuleStudioPage.tsx、api/formationInputClient.ts、components/personal-apps/*、settings/McpPermissions*
- Skills: .agents/skills/{ato-store-publish,store-apply-community,cross-runner-verify,ato-staging-capsule-lifecycle-e2e}、apps/ato/.claude/skills/ato-desktop-mcp-flow
- 既存手順書: apps/ato-api/docs/guides/chatgpt-management-mcp.md

## 付録 B. Detected mismatch / UNVERIFIED

Detected mismatch:
1. chatgpt-management-mcp.md のツール表は 19 tools のまま（現在 39）。source patch / assets / data port / fork / archive が未反映。
2. PWA FormationInputPage は /v1/runtime-network/exploration/:id/input を呼ぶが、ato-api に route が無い（dead path）。
3. PWA の delivery-mode UI は API 実装が無い死んだ並行契約（kd 監査）。
4. scope 不足の応答が HTTP 403 と JSON-RPC error の 2 形態。引数エラーも 2 形態。
5. instance_data:read / instance_operations:execute が同意画面・token にあるがサーバー強制点が見当たらない（安全側だが説明と不一致）。
6. delete_instance_asset が apps:read で destructive。
7. Activity MCP は HTTP 10 tools / stdio 9 tools で不一致。
8. 管理 MCP と Activity MCP で Accept 要件が非対称。
9. staging の INSTANCE_OPERATION_BRIDGE_ENABLED が wrangler.toml に無く、実効値 UNVERIFIED。

UNVERIFIED:
- staging / production の実 tools/list と flag 実効値。
- ChatGPT Personal plugin / Claude での実接続・一連操作（手順書自身が「ChatGPT は未確認」と明記）。
- portable bundle validator 本体の所在（root_contract_ref 再計算がワークスペース外の可能性）。
- ato main の ato run の D 選択実装。
- API に探索 search route が無いことの帰結（hosted で探索がどう公開されるか）。
- staging の formation worker の実バージョン・capacity。
