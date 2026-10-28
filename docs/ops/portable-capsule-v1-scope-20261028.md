# Portable Capsule v1 — スコープ・保証・制限

状態: 草案（Draft PR、2026-10-28 JST）。staging で検証済み、Production へは未適用。
対象: Portable Capsule v1（Private Capsule の Export / Import による K 再現）のスコープと保証範囲。
この文書は実装・設定・データを変更しない。記載は staging と現行コードの観測に限る。

## 0. 文書の位置づけと根拠

> 本文中の `.tmp/...` は作業記録のローカルパス（リポジトリ外）であり、恒久的な参照先ではない。恒久記録はこの文書と各 PR/commit である。

- コード根拠: ato `3d338309`（origin/stg。#1609 の fixture 2 行 + #1610 docs-only。意味論は #1609 の 2 行のみ）、ato-api `d95fab7f`（origin/stg。#941 SSRF fail-closed、#942 MCP export scope、#945 direct-upload envelope をマージ）、ato-pwa `d57e67d2`（origin/stg）。
- 配備済み staging: API `ato-store-staging` version `914ad323-335f-4b91-8463-02b44cd518ed`（stg `d95fab7f`、health 200）、PWA version `e2672da9-f729-4720-a367-dc4eb209d638` / asset `assets/index-DO1Hh8Ko.js`（stg `d57e67d2`）、hosted runner binary `ato-connected-realization-worker-9fbfe8f7`（sha256 `e8816a3c…ef775`、6 slot active）。
- 追加で読んだ作業ブランチ: ato-api `cc37c8f9`/`9340d477`（#941/#942 のブランチ。export/import 関連ファイルは未変更）、ato `17442ceb5`（#1609 の fixture 修正）。
- 本文書は他タスクの証跡を再掲・統合するもので、記載された E2E を本タスクが再実行したものではない。出所を各節に示す。
  - 責任モデル/権限表/在庫: `.tmp/portable-v1-stabilize/audit/LIBRARY-AUDIT.md`、`audit/inventory.json`（30 entries: portable catalogue 8, classic sample 8, durable 14）。
  - E2E: `.tmp/portable-v1-stabilize/e2e/evidence/A-private-app.json`、`B-stateful.json`、`version-before.json`。
  - MCP/継承計画: `.tmp/portable-v1-stabilize/plan/FORMATION-REUSE.md`（本 PR の `docs/ops/agent-native-formation-reuse-20261028.md`）。
  - deploy・Runner・Manifest の読み戻し: `.tmp/portable-v1-stabilize/ops/PHASE1-OPS-REPORT.md`、`ops/deploy-api-staging.log`、`ops/deploy-api-staging-2.log`。
  - negative E2E: `.tmp/portable-v1-stabilize/e2e/evidence/D-negatives.json`、`D-run2.log`。
  - MCP 実機検証（SSRF fail-closed / presigned upload）: `.tmp/portable-v1-stabilize/ssrf-staging/`。
- この文書が主張しないこと: production の状態、任意アプリの意味的等価性、未実行の negative/crash テストの合否、GPU 異環境の再現、`ato.transfer/v1` lane の動作。

## 1. 結論（1 段落）

v1 の保証は「検証済み portable-import provenance を持つ Instance を、別アカウントへ .capsule（wire v3/v4）として持ち出し、受取側で K を再現し、Run receipt で fully_satisfied を確認できる」ことに限定される。export できるのは **portable import 由来の Instance だけ**であり、Formation で新規作成したアプリ、Discover classic Add、保存 reference、public_try の guest import は export 対象外である。形式・digest・closure・receipt の決定権は Rust authority（validator / runtime verifier）にあり、TS は中継と表示のみを行う。

## 2. 中核ルール: export 可能条件と 5 区分

### 2.1 ルール（コードで確定）

`POST /v1/portable-application-exports/instances/:instanceId` は、次を **すべて** 満たす provenance 行が無い場合 `409 portable_export_source_unavailable` を返す（`.tmp/portable-capsule-20261028/api/src/routes/portable_application_exports.ts:326-341`、route 定義 `:277-281`）。

1. `portable_application_imports` に `instance_id` + `owner_user_id` 一致の行があり、`verification_status = verified`
2. `compute_instances` が同じ owner で `status = active`
3. その import 元 `capsule_bundles` が `validation_status = ready`

PWA 側も同じ事実でボタンを出し分ける: `portable_export_available` は「その Instance の portable import 行が verified か」だけで決まる（`api/src/routes/compute_instances.ts:1103-1131`、PWA 表示 `ato-pwa src/components/personal-apps/appDetail/AppTab.tsx:147`、modal `ExportAtoAppAction.tsx:119`）。

### 2.2 区分別の可否（central rule の適用結果）

| 区分 | export 可否 | コードパス（根拠） |
|---|---|---|
| **Imported portable app** | **可** | import が `portable_application_imports` を作る（`capsule_bundles.ts:2105-2110`）ため 2.1 の 1-3 を満たす。実測: A/B E2E の export 成功（§5） |
| **Original Formation app**（Formation で新規作成した Application/Instance） | **不可** | Instance は `POST /v1/computes/:computeId/instances` で作られ（`compute_instances.ts:405-445`）、`portable_application_imports` 行を作らない。よって 2.1 で provenance が null → 409。`portable_export_available` も false |
| **Discover classic Add** | **不可** | Add は既存 Compute+Schema への Instance 作成（`api/src/services/discover_apps.ts:1253-1262` `instanceOfSchema`）で K/D を新規生成せず、import 行も作らない → 2.1 不成立 |
| **Saved reference**（`user_library_entries`） | **不可（そもそも export 対象外）** | 保存は参照のみ（`api/src/routes/user_library.ts:78-94`、読み出しは可視性フィルタ付き join、`LIBRARY-AUDIT.md` §1/§6）。Instance を持たないため `/instances/:id` を呼べない。staging の live 行は 0（監査 §5.1） |
| **public_try guest import** | **不可（account export 対象外）** | public_try は anonymous lane（route `capsule_bundles.ts:2112`、session kind `services/capsule_bundle.ts:629`、`portable_public_try.ts:36`）。export route は `requireAuth("apps:read")` + owner 一致（`:277-281`, `:331-333`）なので account の export には乗らない。staging には verified public_try import が 134 行あるが guest-owned（監査 §5.2） |

> 注: 「Library」という語は 2 つの対象を指す。可視 UI の Library は `/?view=my-apps`（owned instances）であり、サーバの `user_library_entries` は保存参照で専用ページを持たない（`LIBRARY-AUDIT.md` §3 の表）。本節の「Saved reference」は後者。

### 2.3 export / import の状態機械とクラッシュ安全性（既存実装）

- Export: `capturing → queued → claimed → uploading → validating → ready / failed`（`DECISION.md:124`）。冪等キーは capture 前に確保し、capture claim は期限付き、validator claim は agent/claim/expiry で fence、output は D1 batch で validation queue へ（`DECISION.md:128`）。
- Import: `reserved → preparing → running → verified / failed`（`DECISION.md:125`）。予約は `INSERT OR IGNORE`、allocation は一度だけ、reconcile が失敗・中断を回復（`DECISION.md:129`）。同一キー再送は同じ job、異なる入力は 409（`:130`）。
- export の provenance は route 実行時（`portable_application_exports.ts:326-341`）に加え、後段でも再確認される（同ファイル `:575-581`, `:622-625` でも同じ error code）。

### 2.4 v1 が保証する範囲（肯定形）

次は v1 の保証対象であり、A/B E2E で staging 観測がある（§5）。

1. **app-only**: 同一の `.capsule` bytes（wire v3/v4）から、別アカウントで同じ `ContractRef` を再現できる。export は K を変えない（A の K = `sha256:470ac469…` が export 後も一致）。
2. **with-data**: 停止状態で capture した保存データ（browser state / Data Resource / Instance Asset / 停止済み volume checkpoint）を v4 snapshot として束縛し、受取側で同内容を復元できる。snapshot K-prime は `sha256:c020b51c…`（B）。
3. **独立性**: import は受取側の owner で新しい Application/Instance を作り、送信元の認証・session・grant・lease を引き継がない（A の資源への B 読み取りは 4 種すべて拒否）。
4. **receipt**: import 後の実 Run が `verification_status = verified` かつ `receipt.fully_satisfied = true` を満たす（A/B とも）。

## 3. 用語と責任モデル

| 概念 | サーバ実体 | エンドポイント | K/D への影響 |
|---|---|---|---|
| Discover | `discover_apps` / `discover_portable_apps` | `GET /v1/discover/apps`、`POST /v1/discover/apps/:id/add`、`fork`、`try` | 参照のみ。Fork は新 K |
| Library（可視） | `compute_instances`（owned） | `GET /v1/compute-instances` | なし |
| Library（サーバ保存参照） | `user_library_entries` | `GET/POST/DELETE /v1/account/library` | なし（実行・Instance を作らない） |
| Add-from-Discover（classic） | 既存 Compute+Schema 上の新 Instance | `POST /v1/discover/apps/:id/add` | 新 K/D なし |
| Add-from-Discover（portable） | import-app 経路 | `POST /v1/capsule-bundles/:bundleId/import-app` | K 保持、新 Application/Instance |
| Import | 新 Application/Instance + `portable_application_imports` | prepare → PUT → complete → validation → import-app | K は bundle の `root_contract_ref`、D は明示選択 |
| Export | `portable_application_export_jobs` → v4 bundle | `POST /v1/portable-application-exports/instances/:id` ほか | K 保持（app-only）または snapshot K を seal（with-data） |
| Transfer & backup（並行 lane） | `ato.transfer/v1` zip（`transfer_uploads` / `transfer_imports`） | `/v1/transfers/{exports,uploads,imports}`（`api/src/routes/transfers.ts:92-193`） | Portable Capsule とは別形式・別 permission |

Import は「新 Application/Instance を作る」操作であり、Add は「既存 release を解決して Instance を作る」操作である。この差が §2 の export 可否を決める。

### 3.1 4 操作の区別（混同しやすい点）

- **Discover（発見・試用）**: `discover_apps`（classic）と `discover_portable_apps`（portable, 8 行, 全て format 3）の 2 種。Try は公開 listing に対する試用 lane で、portable の Try は `public_try`（anonymous, `capsule_bundles.ts:2112`）として実行され、account export の対象にならない。Discover は K を定義しない。
- **Library（可視）= My Apps**: 可視の Library ボタン/タブは `/?view=my-apps` へ遷移し、`compute_instances` の owned list を表示する（`LIBRARY-AUDIT.md` §3、`ato-pwa` の `GalleryHeader.tsx`, `AccountRoutes.tsx:138-146`）。ここに並ぶのは「所有 Instance」であり、export 可否は §2.1 の provenance で個別に決まる。
- **Library（サーバ保存参照）**: `user_library_entries` は (user, capsule) の**参照**で、実行も Instance 作成もしない（`api/src/routes/user_library.ts:78-94`、read は `services/user_library.ts:241-284`）。専用ページは無く、command palette の recent と legacy 保存に使われる。staging の live 行は **0**（監査 §5.1）。
- **Add-from-Discover**: classic は既存 Compute+Schema への Instance 作成で、K/D を新規生成しない（`discover_apps.ts:1253-1262` `instanceOfSchema`）。portable は `portable_import_required` を経て import-app を実行し、同じ bundle から新 Application/Instance を作る（K 保持）。UI の「Save to Library / 自分用コピー」文言はこの実態（アプリ自体の複製ではない）と一致しない（§6-8）。
- **Import（独立 app/instance）**: `.capsule` を prepare→upload→complete→validation→import-app の順で取り込み、新 Application/Instance と `portable_application_imports` を作る。K は束縛済み `root_contract_ref`、D は `derivation_ref` の明示選択。したがって Import だけが「export 可能な Instance」を生む。

## 4. 不変条件 I1–I8（監査の ID）と実行状況

監査（`LIBRARY-AUDIT.md` §7）は I1–I8 を定義した。DECISION §6 は I1–I7 を定義しており、番号の意味が一部異なるため対応を併記する。

実行コマンド（`api-audit` worktree）:

```sh
cd .tmp/portable-v1-stabilize/api-audit
npx vitest run --project workers \
  src/tests/portable-application-exports.test.ts \
  src/tests/portable-instance-snapshot.test.ts \
  src/tests/portable-instance-snapshot-import.test.ts \
  src/tests/portable-import-recovery.test.ts \
  src/tests/management-mcp-portable-export-import.test.ts \
  src/tests/portable-verification-ingress.test.ts \
  src/tests/capsule-bundle-v4-schema.test.ts
```

結果: **Test Files 7 passed, Tests 57 passed**（`audit/tests-portable-api-audit.log:42-43`）。内訳: exports 3、snapshot 4、snapshot-import 1、import-recovery 19、management-mcp 10、verification-ingress 15、v4-schema 5。

| ID（監査） | DECISION 対応 | 内容 | 主なテスト | 実行区分 | staging 観測 |
|---|---|---|---|---|---|
| I1 | I1 Identity | app-only export で K が変わらない | `portable-application-exports.test.ts:115-116,142,166,490-491,562-563` | unit/worker PASS | **観測**: A で export→B import の `contract_ref` が `sha256:470ac469…` で一致（`A-private-app.json` checks `export_k_preserved`, `B_K_matches`） |
| I2 | I4 State Preservation | with-data は snapshot に束縛された K-prime を検証 | `portable-instance-snapshot.test.ts:388` ほか 4 tests | unit/worker PASS | **観測**: B で K-prime `sha256:c020b51c…` を export→import、状態一致・編集・停止/再開（`B-stateful.json` checks `withdata_Kprime_sealed`, `state_equal`, `stop_wake_persisted`） |
| I3 | I2 Closure | 必要 immutable object が揃い digest/closure が正しい | `capsule-bundle-v4-schema.test.ts` 5 tests | **partial** | 未観測。**closure の権威は Rust validator** であり、監査 worktree では実行できない（`LIBRARY-AUDIT.md` §7 I3） |
| I4 | I3 Independent Authority の一部 | import が新 Application/Instance ID を発行する | `portable-instance-snapshot-import.test.ts:377-380` | unit/worker PASS | **観測**: A/B の import が新 instance/import ID（`new_ids`, `new_instance`, `new_import`） |
| I5 | I3 Independent Authority | A の権限/secret/session/lease を継承しない | `capsule_bundles.ts:1583-1645`（bindings 拒否）+ 監査 §7 I5 | partial | **部分観測**: A の instance/bundle/export/import への B 読み取りは全て拒否（`A-private-app.json` checks `cross_account_denied` 4 項目、`A_instance_404_after_delete`）。DB レベルの grant/secret 比較は未実施 |
| I6 | I5 Verified Realization | receipt は検証済み K/D/environment にのみ有効 | `portable-verification-ingress.test.ts` 15 tests（claim 順序 873、stop 891、503 906、502 919、typed failure 933、attempt fence 818） | unit/worker PASS | **観測**: A/B とも `verification_status=verified`, `receipt_fully_satisfied=true` |
| I7 | I6 Atomic Import | 失敗 import が部分 Application を公開しない | `portable-import-recovery.test.ts` 19 tests | **unit のみ** | 未観測（crash 注入 E2E なし） |
| I8 | I6/I7 の再送面 | retry が冪等（重複 import/export を作らない） | `portable-import-recovery.test.ts`（resume/TTL/duplicate）、`portable-application-exports.test.ts`（capture lease/idempotent）、`management-mcp-portable-export-import.test.ts` | **unit のみ** | 未観測（並行再送の実 E2E なし） |

補足: `portable-verification-ingress.test.ts` の 15 tests は receipt ingress の形式・fencing を対象とし、実 Run の receipt を staging で観測したものではない。A/B の receipt は実 Run のものである。

## 5. Staging E2E 証跡

環境: API `https://staging.api.ato.run`、PWA `https://stg-app.ato.run`、D1 `ato-store-db-stg`。実行時点の tip は `version-before.json` で一致確認（ato `9fbfe8f7f`, ato-api `6ce3827e`, ato-pwa `d57e67d2`）。

| シナリオ | 内容 | K | 主要 ID（短縮） | HTTP / 結果 | 証跡 |
|---|---|---|---|---|---|
| A app-only cross-account | A が bundle を import → app-only export → B が import | 元 K `sha256:470ac469…`（export 後も同一） | A user `pcs_a_a52c9709`、bundle `bnd_01M60DT3…`、A instance `cinst_01M60DT8…`、export `pexp_01M60DTFV9N6…`、export bundle `bnd_01M60DTG…`、B instance `cinst_01M60DV39…` | export start 200、download bundle `sha256:10ceaf34…` 12543 bytes、B root/health 200、cross-account 4 拒否、A 削除後 404 | `e2e/evidence/A-private-app.json`（result PASS、checks 全 true） |
| B with-data cross-account | A が状態作成 → with-data export（K-prime）→ B import → 状態一致・編集・停止/再開 | K-prime `sha256:c020b51c…` | A bundle `bnd_01M60E3W2C…`、A instance `cinst_01M60E40MM…`、export `pexp_01M60E482B…`、export bundle `bnd_01M60E4B9…`、B import instance `cinst_01M60E4WD…` | state add 201/201・GET 200、export start 200、output 32041 bytes `sha256:6dbd11e3…`、B state 200 一致、A 不変 | `e2e/evidence/B-stateful.json`（result PASS） |
| C Discover / Library 境界 | C1: classic Discover listing を Library に保存（参照のみ）。C2: portable Discover listing を import → export → B import | C1: classic listing は K を持たない（`cap_plaza` は publisher の Compute/Schema 参照）。C2: K `sha256:64368f74…3908b`、D `sha256:25fffed8…d61d` | C1: library add 200（`{ok:true,capsule_id}`）、owned instance 0、publisher compute 404。C2: import `cinst_01M60GCAX8J…` verified、export `pexp_01M60GCJN24S…` は v4 で K 保持、B `cinst_01M60GCZTSX…` verified | C1: classic 保存だけでは export を開始できない（owned Instance 不在）。C2: app 200（A）→ B import verified → **B app 200**、K 一致 | `e2e/evidence/C-discover.json`、`C-run.log`（C1/C2 の機能 check は PASS。cleanup は後述の blocker で中断） |
| D negative E2E | 改ざん / 欠落 object / 権限 / 未対応 D / 異なる K / failure 時の部分 app / 冪等 retry | fixture K `sha256:470ac469…`（export 出力側で一致） | A `pcs_da_d52222ce` / B `pcs_db_957fe2d2`、bundle 数点（`bnd_01M60…`） | 改ざん → `hash_mismatch`、欠落 → `unreachable_objects`、K 不一致 → `validator_failed`、unknown profile → prepare 400、未宣言 D → 422 `portable_derivation_not_declared`、B→A instance 404 / export 409 / export-read 404 / bundle 404 / download 409、failed import 0→0、同一 idempotency key 再送で同一 instance/import（行数 1） | `e2e/evidence/D-negatives.json`、`D-run.log`（API `18f5b7a7` で PASS） |

version guard: 実行中の環境同一性は「API worker version + PWA asset」で判定した（repo tip は参考値。ato の docs-only 変更で FAIL にしない）。D の 1 回目は ato tip の docs-only 移動を誤検知したため GUARD FALSE POSITIVE として記録し、製品 FAIL とはしていない。

- A/B は API `9d95482d`（#941 前）で実行した。#941/#942 の変更は MCP の file-fetch と export 作成 scope に限られ、PWA/API の portable export/import 経路を変更しない。よって A/B の結果は現行 `914ad323` にも成立すると判断するが、**同一 version での再実行はしていない**（UNVERIFIED ではなく「別 version での観測」として記録）。
- D は `18f5b7a7`（#941/#942 後、#945 前）で実行し PASS（#945 は MCP direct mode のみ変更するため D の結果に影響しない）。D のうち「非 import Instance の export は 409」は probe の field 誤りと managed pool 枯渇で UNVERIFIED（コード根拠は §2.1/§2.2）。

### 5.1 A/B の主要な観測点（証跡 JSON の抜粋）

| 観測 | A（app-only） | B（with-data） |
|---|---|---|
| 元 bundle | `bnd_01M60DT3…`, validation ready, v3, profile `ato.portable-application/1`, D 2 本（python + oci） | 同 fixture を再 import: `bnd_01M60E3W2C…`, ready, K `sha256:470ac469…` |
| A import | instance `cinst_01M60DT8…`, import `pai_01M60DT8VNQ6…`, verified, receipt true, app root 200 / health 200 | instance `cinst_01M60E40MM…`, import `pai_01M60E40PXAK…`, verified, root 200 |
| export 出力 | `pexp_01M60DTFV9N6…`, `bnd_01M60DTG…`, `bundle_sha256` `sha256:10ceaf34…`, 12543 bytes, `contract_ref` `sha256:470ac469…`, envelope v4/profile 2 | `pexp_01M60E482B…`, `bnd_01M60E4B9…`, `bundle_sha256` `sha256:6dbd11e3…`, 32041 bytes, K-prime `sha256:c020b51c…`, envelope v4/profile 2, snapshot あり |
| B import | instance `cinst_01M60DV39…`, import `pai_01M60DV3B89…`, verified, receipt true, root 200 / health 200, `new_instance`/`new_import` true | instance `cinst_01M60E4WD…`, import `pai_01M60E4XH4…`, verified, receipt true, state GET 200 で A と完全一致 |
| 独立性・不変性 | B から A の instance/bundle/export/import 読み取り 4 種すべて拒否、A 削除後 404 | 状態編集・停止/再開後も内容保持、A 側 instance は不変 |

> A/B の証跡 JSON は result PASS（boolean leaf 全 true）。本文書はこれを再実行せず引用している。fixture bytes は A の `sha256:342e26ac…`、K 期待値は `sha256:470ac469…`。


### 5.2 Phase 1 P0 の検証と配備（本タスク）

| 項目 | 変更 | 検証（実行済み） | 配備・状態 |
|---|---|---|---|
| MCP file-fetch の SSRF fail-closed | ato-api #941（`34503880`）+ #945（`d95fab7f`）: サーバ側 fetch を platform 管理ホストの完全一致 allowlist に限定し、DoH で A/AAAA の private/reserved を拒否。任意 URL は fetch せず、既存 presign 経路（`prepare_import` の `direct_upload:true` + `complete_import_upload`）へ fail closed。#945 は direct mode に `format_version`/`profile` を必須化（実機で NULL envelope が validator に claim されない不具合を修正） | unit: MCP + compute-source = 50 passed（#945 後 36 passed を含む）。staging 実機（delegated MCP token、final version `914ad323`）: **20/20 PASS** — untrusted host → `file_download_url_untrusted` + `next_action` + bundle 行 0→0、IP literal/http → `file_download_url_invalid`、allowlist host → `file_download_failed`（=allowlist+DoH 通過）、direct prepare → presigned R2 URL（`upload_direct:true`）→ PUT 200 → complete → **validation ready（format 3 / `/1`, K `470ac469…`）→ import_app(python) → `verified` / `fully_satisfied=true` → app 200・`/api/notes` 200**、stored digest/size/envelope 一致、write token の export は `portable_export_source_unavailable`、**read-only token の export は 403 `insufficient_scope`（jobs 0）**、run 前後で version 不変 | API staging version `914ad323`（stg `d95fab7f`、最終） |
| MCP export の scope 境界 | ato-api #942（`90b277ae`）: `export_app` を `apps:write` 必須に変更（`get_export`/`check_library_exportability` は `apps:read` のまま）。`ato.transfer/v1` の「apps:read token に bundle を渡さない」規則と整合 | unit: 同 test file 35 passed（read-only → `insufficient_scope`/job 行なし、write → route 到達、read tools は維持） | API staging version `18f5b7a7`（2026-10-28T05:33Z）。#945 後は `914ad323` |
| Rust test 復旧 | ato #1609（`4988e265`）: `StateAttachmentV1.backing` の test fixture 2 箇所に `backing: None`（production/serde/canonical bytes は不変） | macOS: `ato-ipc` 84+4+16(+1 ignored)+23 passed / `ato-runtime-attempt` 197 passed。Linux 実機（ubuntu-sugamo）: `network_bridge::deny_bridge_tests::binds_a_private_socket_under_a_path_longer_than_sun_len` = 1 passed（`network_bridge::` 3 passed） | コードのみ（runner 再配備なし） |
| python process 実 Runner E2E | 変更なし（stg の #1608 を使用） | 新規 `pcr_` account: validation ready → import-app（python D 明示選択）→ `verification=verified`, `receipt.fully_satisfied=true`, `realization=process`, `contract_ref=sha256:470ac469…`, app 200・`/api/notes` 200 | runner `9fbfe8f7`（slot1–6 active） |
| Runner / staging 衛生 | 変更なし | slot1–6 active（2026-10-28T04:19:42Z 以降）、binary sha256 `e8816a3c…ef775` = 期待値、journal warning なし、rollback 用 drop-in backup + 旧 binary 保持。前回 session の `pcv1_*` 残骸 251 行を FK-safe に削除（削除前 snapshot あり）。immutable な `capsule_bundles` 77 行（69 ready / 4 rejected / 4 uploading）は設計上保持 | rollback 可 |
| Release Manifest 照合 | Manifest は編集しない | API/PWA/Runner の実配備を読み戻し: manifest pin（ato `49b0f9627` / api `db7cd6fbad15` / pwa `73c266dfc52f` / api_version `49179b15` / asset `index-BbFl-btK.js`）は **STALE/MISMATCH**（staging が main より先行するため想定内）。admin `1620d881` PASS。migration は required 7 件すべて ledger に存在し、stg `90b277ae` で drizzle 373 ファイル・未適用 0（journal だけでは不十分なためファイル単位で照合、ledger 397 との差分 24 は historical 名）。contracts は capsule `2` / K / d_old PASS、d_new `16cea2b3` と runtime/api_schema は UNVERIFIED。`runner_build_id` は runner が build id/commit を公開しないため UNVERIFIED（改善提案のみ） | PROPOSAL / NOT APPLIED の pin 案を ops report に記録 |

MANIFEST の詳細（PASS/MISMATCH/UNVERIFIED の全行、生コマンド、cleanup の before/after、disk 98%（19G free）の risk、rollback pointer）は `.tmp/portable-v1-stabilize/ops/PHASE1-OPS-REPORT.md` を参照。

### 5.3 本タスクのマージ・配備状況（Production は未変更）

| repo | PR | merge commit | staging 反映 |
|---|---|---|---|
| ato | #1609（fixture 復旧、Draft 解除して merge） | `4988e265`（stg はその後 docs-only #1610 で `3d338309`） | コードのみ（deploy なし） |
| ato-api | #941（SSRF fail-closed） | `34503880` | API `da3b1a3b` |
| ato-api | #942（MCP export scope） | `90b277ae` | API `18f5b7a7` |
| ato-api | #945（direct upload の envelope metadata 必須化） | `d95fab7f` | API `914ad323`（最終） |

Production への deploy・migration・flag/secret 変更・CI 実装は行っていない。force push は #942 の rebase 時の `--force-with-lease` のみ。

## 6. 未対応・制約（v1 で保証しないこと）

1. **Original Formation app の export**: §2.1 の provenance が無いため不可（409）。フォーメーションで作ったアプリを配布するには「export できる Instance」を作る別経路が必要で、v1 のスコープ外。
2. **secrets / bindings の移送**: import は宣言外・operator 管理 binding を拒否する（`api/src/routes/capsule_bundles.ts:1583-1645`）。staging 実測では `instance_data_bindings` 22 行すべて revoked、`managed_secret_policies` 0（監査 §5.1）。secret 値は v1 で移送しない。
3. **process memory**: 移送しない（DECISION §0/§12、`DECISION.md:16,71`）。
4. **GPU 異環境保証**: 保証しない。portable catalogue には GPU 要求アプリ（Wan Animation: nvidia 1x, 46GiB VRAM 等）があるが、GPU 間の再現 E2E は未実施（`inventory.json` の `host` 情報、監査 §5.4）。
5. **v2 computation-root の読み替え**: しない。decoder は version で明示分岐し（`.tmp/portable-capsule-20261028/ato/lib/objects/src/portable_bundle.rs:241-262`）、v2 を v3/v4 として解釈しない。
6. **filesystem volume の with-data**: **コードは実装済みだが E2E 未検証で、DECISION と矛盾する**。DECISION §12 は「filesystem volume の with-data 保証は v1 対象外」（`DECISION.md:182`）、I4 の現状欄は「実装済み（volume は対象外）」（`:112`）。一方コードは停止済み volume checkpoint の export（`portable_instance_export.ts:305` の `state_volume_checkpoint_required`、route 側 `:396-430`）と `ato.state.filesystem@1` 復元（`portable_instance_snapshot.ts:212-260`）を持つ。**判断が必要**（コードを制限するか DECISION を更新するか）。
7. **2 本の転送 lane**: Portable Capsule（`.capsule` v3/v4）と `ato.transfer/v1` zip が staging で併存する。後者は origin/main 由来（`e6ab0f38`, `463f478d`）で、first-party session + same-origin のみ、`apps:read` token では取得できない設計（`api/src/routes/transfers.ts:9-13`）。staging は `TRANSFERS_ENABLED=true`（`api/wrangler.toml:215,478`）。どちらが「Library export」の正規 UX かは**製品判断**（監査 §4.1/§8）。
8. **Library の命名・コピー文言の不整合**: 可視 Library は My Apps、サーバ Library は保存参照。加えて「Save to Library makes your own copy」は API の実態（既存 release の Instance 作成または同一 bundle の import）と一致しない（監査 §3/§4）。
9. **保存 reference の silent hide**: `user_library_entries` は FK なし・可視性フィルタ付き INNER JOIN のため、blocked/unlisted/削除で**エラーなく消える**。実装上 capsules の hard-delete 経路は無く、現実の症状は「Library/recents から静かに消える」。orphan 行は API から削除できない（`DELETE` は解決不能時に 200 no-op、`routes/user_library.ts:180-210`、監査 §6）。v1 で修正しない。
10. **public_try は account export 対象外**（§2.2）。guest の作業を export する経路は v1 に無い。

### 6.1 DECISION と実装/証跡の差分（要判断）

| 論点 | DECISION の記述 | 実装/証跡 | 状態 |
|---|---|---|---|
| filesystem volume with-data | 「v1 対象外」（`DECISION.md:16,71,182`）、I4 は「実装済み（volume は対象外）」（`:112`） | 停止済み volume checkpoint の export/restore が実装（`portable_instance_export.ts:305,396-430`, `portable_instance_snapshot.ts:212-260`） | **不一致**。E2E 未検証。制限するか DECISION を更新するか要判断 |
| cross-account E2E | §12 で UNVERIFIED（`DECISION.md:179`） | A/B の staging E2E が PASS（§5、`e2e/evidence/`） | **一部解消**（app-only と with-data の 2 シナリオのみ） |
| v4 import の hosted 経路 | §12 で UNVERIFIED（`DECISION.md:181`） | B が v4/profile 2 の bundle を import し verified（§5） | **解消**（with-data シナリオ内） |
| Export Artifact の MCP 経由取得 | §12 で未確定（`DECISION.md:183`） | MCP export tool は `download_url` を返し、bytes は認証付き fetch 前提（`management_mcp.ts:897`、`FORMATION-REUSE.md` §4） | **未確定のまま**（E2E 未実施） |

## 7. 次フェーズ（Remote MCP からの K→D→receipt）

既存 Rust 機構でループは成立しており、必要なのは小さな read/selection 表面である。詳細: `.tmp/portable-v1-stabilize/plan/FORMATION-REUSE.md`。

7 ギャップ（1 行ずつ）:

1. **D enumeration が未公開**: formation は選択済み derivation_ref のみ、export は bundle 単位の derivation_refs、`get_app_schema` は K/D を返さない。
2. **seal 前の K 候補読み取りが無い**: ready 後の `get_formation_job.result` のみ（`job_outcome.ts:81-96`）。
3. **frozen-K reform の user API が無い**: CLI `ato reform` と admin reformation のみ（`apps/cli/src/lib.rs:2659-2663`, `api/src/routes/admin.ts:959`）。
4. **Runtime-Network 探索が CLI 専用**: `ato form --runtime-network` 相当が MCP に無い。
5. **receipt 確認が間接的**: `wait_formation_job` は compact summary、full receipt は `get_formation_job` / `get_import`。
6. **object-graph の private 登録が MCP に無い**: `/v1/capsule-object-graphs` + CLI `ato upload` のみ。
7. **認証境界**: MCP は management OAuth（`apps:read`/`apps:write`）、user-owned Runtime token は別設計。

最小提案（新 engine なし）:

- (a) formation job result の bounded read tool（`contract_ref` + observation の id/result のみ）
- (b) 宣言済み D の列挙 read（bundle/formation result に既にある ref を返すだけ、新 table なし）
- (c) `get_formation_job` / `get_import` を receipt 確認の正式手順として文書化（`wait_*` だけに依存しない）
- (d) frozen-K reform と runtime-network 探索は要件判断後に、既存 SatisfyRequest / binding を包む tool として追加（新 planner は作らない）

## 8. 付録: 再現・確認コマンド

### 8.1 staging E2E

```sh
cd /Users/egamikohsuke/Ekoh/projects/ato-run
# A app-only cross-account（結果: e2e/evidence/A-private-app.json）
node .tmp/portable-v1-stabilize/e2e/sA-private-app.mjs
# B with-data cross-account（結果: e2e/evidence/B-stateful.json）
node .tmp/portable-v1-stabilize/e2e/sB-stateful.mjs
# C は未実施（該当スクリプト無し）。D はスクリプトのみ存在
node .tmp/portable-v1-stabilize/e2e/sD-negatives.mjs   # NOT RUN として扱う
```

補足: `e2e/lib.mjs` の `sql()` は `wrangler d1 execute ato-store-db-stg --remote --env staging` を実行する（テストユーザー作成・後始末用）。write を伴うため Lead 承認の下でのみ実行する。

### 8.2 API vitest（不変条件）

```sh
cd .tmp/portable-v1-stabilize/api-audit
npx vitest run --project workers \
  src/tests/portable-application-exports.test.ts \
  src/tests/portable-instance-snapshot.test.ts \
  src/tests/portable-instance-snapshot-import.test.ts \
  src/tests/portable-import-recovery.test.ts \
  src/tests/management-mcp-portable-export-import.test.ts \
  src/tests/portable-verification-ingress.test.ts \
  src/tests/capsule-bundle-v4-schema.test.ts
# 期待: Test Files 7 passed, Tests 57 passed
```

### 8.3 deploy read-back

```sh
# PR 状態（#941/#942 merged、#1609 merged）
gh -R ato-run/ato-api pr view 941 --json state,mergeCommit,mergedAt   # MERGED 34503880b
gh -R ato-run/ato-api pr view 942 --json state,mergeCommit           # MERGED 90b277ae
gh -R ato-run/ato pr view 1609 --json state,mergeCommit              # MERGED 4988e265
# 反映確認（deploy log の Postflight 行と health）
grep -E 'Deployed ato-store-staging|Postflight|Health check|LOCK_' .tmp/portable-v1-stabilize/ops/deploy-api-staging-3.log
# active=914ad323-335f-4b91-8463-02b44cd518ed, secrets=37, LOCK_RELEASED
curl -s https://staging.api.ato.run/health                            # {"status":"ok",...}
```

### 8.4 rollback pointer（未実行）

- deploy は `pnpm deploy:staging` → `scripts/deploy-staging-locked.mjs` → `scripts/deploy-worker-safe.mjs`（`ops/deploy-api-staging.log` / `-2.log` に lock token と release SHA を記録）。
- deploy 前後の active version: `9d95482d…`（#941 前）→ `da3b1a3b…`（#941）→ `18f5b7a7…`（#942）→ `914ad323…`（#945、最終）。各 log の Preflight/Postflight 行に記録。
- Runner: 切替前 drop-in は `/home/ekohsuke/runner-dropins-backup-20261028c-slot1..6`、旧 binary `ato-connected-realization-worker-3b6a1df6e` を保持（`ops/PHASE1-OPS-REPORT.md`）。
- D1 cleanup: 削除した `pcv1_*` 行は削除前 snapshot を `ops/deleted-rows/*.json` に保持。immutable な `capsule_bundles` は削除していない。
- **rollback 手順は本タスクで実行・検証していない**（コマンドを推測して記載しない）。実運用の rollback は ops の既存手順に従うこと。

### 8.5 audit 読み取りクエリ（read-only）

```sh
node .tmp/portable-v1-stabilize/audit/schema.mjs
node .tmp/portable-v1-stabilize/audit/summarize.mjs
node .tmp/portable-v1-stabilize/audit/portable-catalogue.mjs
node .tmp/portable-v1-stabilize/audit/durable-query.mjs
```

## 9. UNVERIFIED / NOT RUN 一覧

| 項目 | 状態 | 理由 |
|---|---|---|
| C: Discover / Library 境界 | **PASS（C1/C2 の機能 check）** | C1: classic 保存 → library add 200 / owned instance 0 / publisher compute 404。C2: portable listing → import verified → app 200 → export v4 K 保持 → B import verified → B app 200・K 一致。cleanup は `state_volume_retained` 409 で中断（製品挙動ではなく後片付け blocker） |
| D: negative E2E | **PASS（必須 7 項目）** | 改ざん/欠落/権限/未宣言 D/異なる K/no partial/冪等 retry。API `18f5b7a7` で実行 |
| D: 非 import Instance の export 409（provenance 制限の実測） | **UNVERIFIED** | probe の field 誤り + managed pool 枯渇。コード根拠は §2.1/§2.2 |
| pcs_*/pssrf_* テスト残骸の完全削除 | **NOT COMPLETED** | users/imports/exports/runs は削除済み。instances/slots/bundles は D1 の trigger（validated bundle immutable / state volume retained / FK）で削除不可。すべて削除済みテストユーザー所有で実ユーザー影響なし。id は `e2e/evidence/residue-ids.json` と `ssrf-staging/evidence-final.json` |
| I3 closure の実証（Rust validator の canonical/digest/closure 負例） | **NOT RUN** | 監査 worktree では Rust validator を実行できない。TS fixture は権威ではない |
| I7/I8 の crash/retry 実観測 | **NOT RUN** | unit のみ。実 staging への crash 注入なし |
| I5 の DB レベル権限非継承（B に A の grant/secret が無いことの比較） | **NOT VERIFIED** | API 読み取り拒否は観測、DB 比較は未実施 |
| filesystem volume の with-data 実 E2E | **NOT RUN** | コードは存在するが staging 未観測。DECISION §12 と不一致 |
| GPU 異環境（GPU→non-GPU / 異 GPU）再現 | **NOT VERIFIED** | GPU 要求アプリは catalogue にあるが E2E なし |
| MCP 経由の import E2E | **PASS** | delegated token で direct upload → validation ready → import → receipt verified → app 200（20/20）。export は write token で provenance 409、read-only token で 403 を実測。export 成果物の MCP 経由ダウンロード（bytes）は未実行 |
| `ato.transfer/v1` lane の動作確認 | **NOT RUN** | 併存 lane。flag は staging で true だが実行していない |
| Library saved-ref の消失挙動の実観測 | **部分観測** | C1 で保存参照 1 行を作成（`cap_plaza`）。参照先の可視性喪失による silent hide 自体は未観測（コード根拠は監査 §6） |
| filesystem volume の with-data 実 E2E | **NOT RUN** | C2 の instance は volume を持ち cleanup で `state_volume_retained` を観測したが、with-data export の実 E2E は未実施 |
| production の状態 | **NOT IN SCOPE** | 本文書は staging のみ |

## 10. 参照

- `.tmp/portable-v1-stabilize/audit/LIBRARY-AUDIT.md`（責任モデル、権限表、surface map、saved-ref risk、I1–I8 と 57 tests）
- `.tmp/portable-v1-stabilize/audit/inventory.json`（30 entries: 8 portable catalogue / 8 classic sample / 14 durable、K/D/route/host/GPU/model-set/bindings/closure）
- `.tmp/portable-v1-stabilize/e2e/evidence/`（`A-private-app.json`、`B-stateful.json`、`D-negatives.json`、`version-before.json`、`RESULTS.md`）と `.tmp/portable-v1-stabilize/e2e/` のスクリプト
- `.tmp/portable-v1-stabilize/ops/PHASE1-OPS-REPORT.md`（Runner/Manifest/cleanup/rollback/disk）と `ops/deploy-api-staging*.log`
- `.tmp/portable-v1-stabilize/ssrf-staging/`（MCP 実機の fail-closed と presigned upload 証跡）
- `.tmp/portable-v1-stabilize/plan/FORMATION-REUSE.md`（MCP 再利用計画、マッピング 11 行。本 PR の `docs/ops/agent-native-formation-reuse-20261028.md`）
- `.tmp/portable-capsule-20261028/DECISION.md`（§6 I1–I7、§11 実装/検証、§12 未対応/UNVERIFIED）
- PR: ato#1609（merged `4988e265`）、ato-api#941（merged `34503880`）、ato-api#942（merged `90b277ae`）、ato-api#945（merged `d95fab7f`）。配備: API `ato-store-staging` version `914ad323-335f-4b91-8463-02b44cd518ed`。
