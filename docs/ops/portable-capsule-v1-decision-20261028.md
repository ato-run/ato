# Portable Capsule v1 — 形式選定と不変条件（2026-10-28）

状態: docs-only 決定記録。実装変更なし。
対象: 異なる ato.run アカウント間で Private Capsule を Export / Import し、同じ K を再現できること（PWA↔PWA）。
範囲外: PWA↔ローカル、CPU/GPU 異種環境、Formation Worker 最適化、Production 変更、CI 実装。

## 0. 結論

**新しい .capsule 形式は導入しない。** 既存の versioned capsule bundle（wire v3 = ato.portable-application/1、wire v4 = ato.portable-application/2）をそのまま Export Artifact として採用する。形式選定は選択肢 A（既存形式の再利用）で、B（最小拡張）・C（汎用 archive + manifest）・D（新 wire 導入）は現時点の要件では不要。

理由:
- K/D identity、object closure、state/asset、cross-account import、atomicity は既存 v3/v4 と既存 export/import job で表現・検証できる。新しい形式は既存 validator / receipt / bundle table と二重実装になる。
- Export の成果物は既に v4 bundle（profile ato.portable-application/2、portability manifest、任意の instance_snapshot_ref）として生成され、既存の capsule_bundles validation → import-app 経路で Import できる。
- v2 の ComputationRef と v3/v4 の ContractRef は decoder が version で分岐し、読み替えない（既存実装）。

v1 の保証対象は「app-only（K/D の再現）」と「with-data（browser state / Data Resource / Instance Asset の snapshot）」に限定する。filesystem volume / process memory / bindings / secrets は後続。

## 1. 概念の分離

| 概念 | 定義 | 既存の実体 |
|---|---|---|
| Capsule | K によって同一性が定義される計算 | ContractRef = sha256(JCS(BoundContract))（wire v3/v4） |
| Derivation (D) | K を実現する経路 | DerivationRef = sha256(JCS(BoundDerivation))。bundle の derivations[] |
| Bundle | K / D / 必要な object closure の論理的集合 | PortableApplicationBundle（index + payloads） |
| Export Artifact | Bundle を持ち運ぶ具体ファイル | 同じ .capsule bytes（MIME application/vnd.ato.capsule） |
| Application / Instance | ato.run の所有・実行・状態管理単位 | capsules / discover listing / compute_instances / portable_application_imports |

Identity はファイル形式・転送方法・アカウントに依存しない。bundle の SHA-256 は transport integrity と provenance のみで、K の代用にしない。

## 2. 既存形式の棚卸し

| 形式 | version/profile | ルート | 用途 | 状態 |
|---|---|---|---|---|
| v2 | 2 / computation-root | root_computation_ref | 旧 computation capsule | 既存 reader 維持。v3 へ読み替えない |
| v3 | 3 / ato.portable-application/1 | root_contract_ref | contract-root、完全埋め込み closure。static / process / OCI | 実装済み。CLI / PWA / hosted import |
| v4 | 4 / ato.portable-application/2 | root_contract_ref + portability manifest + optional instance_snapshot_ref | dependency transport（thin/cached/offline）、state/asset snapshot | 実装済み。hosted export の出力形式 |

根拠:
- decoder dispatch: lib/objects/src/portable_bundle.rs（version 2 → ComputationV2、3 → PortableApplicationV3、4 → PortableApplicationV4）。unknown profile は fail closed。
- v3 RFC: docs/rfcs/draft/PORTABLE_APPLICATION_BUNDLE_V3.md（K = root_contract_ref、完全 closure、hosted import は source ZIP に戻さず static application として materialize）。
- v4 RFC: docs/rfcs/draft/PORTABLE_DEPENDENCY_EXPORT.md（packing は transport 決定で K/D を変えない。thin/cached/offline、offline は external object 禁止）。
- export RFC: ato-api docs/rfcs/draft/portable-application-export.md（capture fence、job lifecycle、asset rebinding、limits）。
- export/import 実装: ato-api src/routes/portable_application_exports.ts、src/routes/capsule_bundles.ts、src/services/portable_instance_export.ts、src/services/portable_instance_snapshot.ts。
- PWA: ExportAtoAppAction（include saved data / portability / volume checkpoint）、CreateAppPage の .capsule import、api/portableApplicationExportClient.ts、api/portableApplicationClient.ts。
- Rust authority: apps/portable-application/src/{hosted_export,instance_snapshot,portability_export,portability_plan}.rs、validator（ato-portable-validator）。

## 3. 選択肢の評価

評価軸: K/D identity、closure、state/asset、cross-account safety、既存 reader/writer 互換、Rust/TS 重複、実装・保守・移行コスト、CPU/GPU 後続。

| 案 | 内容 | 判定 | 理由 |
|---|---|---|---|
| A | 既存 v3/v4 bundle をそのまま Export Artifact に | **採用** | 既に export 出力が v4、import が v3/v4 を受理。K/D 維持、closure 検証、snapshot、receipt が既存。追加 codec 不要 |
| B | 既存形式の最小拡張 | 不採用（保留） | v1 の要件は v3/v4 で満たす。拡張が必要になる具体的反例が無い。将来、with-data の filesystem volume を保証する時に v4 の snapshot schema を拡張する可能性はあるが、新 wire version は不要 |
| C | 汎用 archive（tar/zip）+ manifest で移送 | 不採用 | closure の canonical bytes、object ordering、digest 参照、K/D binding を再実装することになる。専用 manifest は結局 v3/v4 の index と同型になり、二重 canonicalizer になる。標準 archive は transport には使えるが、意味の authority にはならない |
| D | 新しい wire v3 を導入 | 不採用（既に存在） | v3（ato.portable-application/1）は既に実装済み。これを超える新 version を今作る根拠が無い。v2 → v3 の読み替えは禁止 |

新形式が不可避になる条件（現時点で該当なし）:
1. v3/v4 で表現できない K/D 関係が必要になった。
2. closure の完全性を v3/v4 の schema で証明できない object 種別が増えた。
3. cross-account で receiver が検証できない state/asset 種別が必須になった。
4. 既存 reader の互換を壊さずに field を追加できない（その場合も version bump で足りるか先に検討）。

## 4. YAGNI（今回作らない）

- 新しい汎用 archive framework / 新しい manifest 形式。
- 任意 OS / GPU / 大規模モデル向け streaming transport。
- 自動 Derivation 最適化 / 新しい Formation Engine / D scheduler。
- TypeScript 側の canonicalizer / ContractRef 再計算 / validator の再実装。
- K/D の意味的等価性の一般証明。
- filesystem volume / process memory / bindings / secrets の移送。
- 将来用途だけの metadata / extension point。
- 独自の MCP Import Engine（既存 service を呼ぶ tool のみ）。

## 5. 最小形式仕様（v1）

### 5.1 Export Artifact

- 1 ファイル。拡張子 .capsule、MIME application/vnd.ato.capsule。
- JSON envelope: index + payloads。index は version / profile / root_contract_ref / application_ref / derivations[] / objects[] / 任意の instance_snapshot_ref / 任意の portability。
- Export job の出力は **v4（ato.portable-application/2）**。app-only でも portability manifest を持つ（thin / cached / offline）。
- with-data のとき instance_snapshot_ref が追加され、snapshot は K に bind された observation として扱う。
- 完全な closure: index.objects は reachable set と一致。外部 object は portability manifest が明示（offline では禁止）。
- 256 MiB transport 上限、objects 10,000、payload 64 MiB、decoded 256 MiB（既存 limits）。

### 5.2 Import

- 受理 version: 3 または 4。v2 は Portable Application import では拒否。
- prepare → PUT content → complete → validation job（Rust validator agent）→ import-app。
- validation は canonical encoding、payload digest、descriptor size、reference closure、external object set、snapshot binding を検証する。
- import-app は新しい Application / Instance を作る。既存 import の receipt / lease / secret / grant を継承しない。
- 同一 idempotency key の再送は同じ結果、異なる入力は 409。reconcile で失敗・中断を回復する。

### 5.3 拒否条件

- version 不明 / profile 不明 / v2 を portable として解釈。
- payload digest 不一致、size 不一致、closure 欠落、重複 reference、未宣言 external object。
- offline なのに external object がある。
- instance_snapshot が K の observation と一致しない。
- 改ざんされた receipt / bundle_sha256。
- 受取側で capability 不足（version ではなく admission の問題として別コード）。

## 6. 不変条件（I1–I7）

表記: Export(A, E, mode) → artifact。Import(B, artifact) → ApplicationB, InstanceB。Execute(D, Target) → C。Verify(C, K) → Receipt。

| ID | 事前条件 | 事後条件 | 責務 | 検証方法 | 現状 |
|---|---|---|---|---|---|
| I1 Identity | artifact が v3/v4、index.root_contract_ref が canonical BoundContract の digest | Import 後の app の K = 元の K（app-only） | Rust validator（decode / digest）、API は保持のみ | ContractRef 比較、bundle の root_contract_ref | 実装済み（要 E2E 証跡） |
| I2 Closure | index の objects が宣言され payload が揃う | 必要 object が欠落せず、digest/size/参照関係が一致 | Rust validator（canonical / reachable set） | 欠落・改ざん negative test、reachable set 比較 | 実装済み |
| I3 Independent Authority | Import は B の認証で実行 | B の新 Application/Instance。A の Secrets/Grant/Token/Lease を継承しない | API（owner / auth）、DB（新 row） | A の cookie/token を使わない E2E、DB 検査 | 実装済み（要 E2E） |
| I4 State Preservation | with-data の capture fence が一致 | snapshot と Asset が整合し、B で再現。必要なら新しい Snapshot K を seal | Rust validator（snapshot bind）、API（capture / asset copy） | data/asset digest 比較、編集・再起動・再 Export | 実装済み（volume は対象外） |
| I5 Verified Realization | Import 後の run が対象 K を実行 | Receipt が fully_satisfied / verification_status=verified | Runtime verifier（実際の HTTP / observation） | 実 Run の receipt。format validation だけでは PASS にしない | 実装済み（import-app の receipt） |
| I6 Atomic Import | 失敗・crash・retry | 不完全 Instance / 重複 Import を残さない | API（idempotency / claim / reconcile） | crash/retry negative test、重複 POST | 実装済み（要 E2E） |
| I7 Privacy | Private Capsule | Discover 公開や A の権限付与に繋がらない | API（private default、publish は別操作） | B の listing / visibility 検査 | 実装済み |

証明責務の分離:
- 形式的に証明できる性質: 形式の canonical encoding、digest、closure、descriptor 整合（Rust validator）。
- Verifier に依存する性質: 実際の Run が K を満たすこと（runtime verification receipt）。「任意のアプリの意味的等価性」は主張しない。
- 製品の認可: ownership / account / publish / secrets（API と product policy）。ここは形式仕様の外。

## 7. 状態機械とクラッシュ安全性

Export（既存）: capturing → queued → claimed → uploading → validating → ready / failed。
Import（既存）: reserved → preparing → running → verified / failed。cancel / reconcile / retry は idempotency key と claim fence で直列化。

クラッシュ安全性:
- Export: 冪等キーを capture 前に確保。capture claim は期限付き。validator claim は agent / claim / expiry で fence。uploaded output は D1 batch で validation queue に遷移。
- Import: 予約 row を INSERT OR IGNORE で確保。allocation は一度だけ。reconcile が失敗・中断を回復。
- 重複: 同一キーの再送は同じ job、異なる入力は 409。

TLA+ 等のモデル検査: v1 では導入しない。既存の D1 CAS + idempotency + claim fence で直列化され、新しい並行プロトコルを追加しないため。ただし E4 で crash / retry / concurrent import の負例を実測し、未検証の race が残る場合は別途モデル化する。

## 8. Discover / Library の責務

| 機能 | 責務 | しないこと |
|---|---|---|
| Discover | 公開 Application の発見・体験。Add は既存 Release を解決する | Add のたびに新しい K/D を生成しない |
| Library | 自分の Application と追加した Application の管理、保存 Release の識別、Export、closure の取得可能性の保証 | Add で即時に実体複製しない（参照でよい） |

保持条件:
- Export 可能なのは verified portable import を持つ Instance（portable_export_available）。
- Export は source bundle と instance snapshot を読む。source bundle が無い場合は明示エラーにし、黙って別 Release に差し替えない。
- 既存 Library 項目を条件未達という理由で黙って削除しない。Export 不可の理由を返す。
- ライセンスは Discover listing / instance の metadata を保持し、Export に含める（公開範囲の判定には使わない）。

## 9. 既存ギャップ（本監査時点）

1. **MCP tools**: #936 で追加済み（§11）。
2. **cross-account E2E**: app-only は §11.2 で PASS。with-data とブラウザ UI は未検証（§11.4）。
3. **with-data の範囲**: browser state / Data Resource / Instance Asset は実装済み。filesystem volume / process memory / bindings / secrets は対象外。
4. **保持ポリシー**: capsule_bundles の cleanup は uploading 期限切れのみ。validated bundle の長期保持・quota eviction は UNVERIFIED。
5. **docs**: export/import の user-facing 手順と保証範囲が未整理。

## 10. 最小実装計画（形式変更なし）

1. **MCP tools（P0）**: 既存 REST を呼ぶ薄い tool。
   - check_library_exportability（instance の portable_export_available / 理由）
   - export_app（instance_id, include_saved_data, portability, idempotency_key）→ export job
   - get_export（job 状態 + download_url）
   - validate_import（file param または bundle upload）→ 事前検証
   - import_app（bundle_id, derivation_ref, idempotency_key）→ 新 Instance
   - get_import（status / verification_receipt）
   独立の Import Engine / Canonicalizer は作らない。
2. **E2E**: Portable Notes fixture で E1–E4。
3. **docs**: PWA 手順、Export Artifact の説明、未対応条件。

## 11. 実装・検証結果（2026-10-28 追記）

### 11.1 実装

| PR | 内容 | 状態 |
|---|---|---|
| ato #1607 | 本 docs-only 決定記録 | Draft |
| ato-api #936 | 管理 MCP に Portable Capsule の export/import tools を追加。capsule-bundle owner routes を delegated OAuth scope（apps:read/apps:write）で許可 | マージ済み・staging 配備済み |
| ato-api #937 | delegated MCP bearer を run-mutation Origin gate に通す（Origin を持たない MCP 経由の export/import のため）。malformed token は従来どおり拒否 | マージ済み・staging 配備済み |
| ato-api #938 | MCP の file download を HTTPS/userinfo/private host/redirect/timeout で制限 | マージ済み・staging 配備済み |
| ato-api #939 | MCP envelope の off-by-one 修正 | マージ済み・staging 配備済み |
| ato-api #940 | MCP 内部 upload URL から /v1 を除去 | マージ済み・staging 配備済み |
| ato-pwa #558 | 単一 realization の capsule を既定 route で import、複数は候補を提示 | マージ済み・staging 配備済み |
| ato-pwa #559 | Add App フォームを session 解決後に描画（ERR_ABORTED の原因除去） | マージ済み・staging 配備済み |
| ato-pwa #560 | サインイン文言を既存カタログに統一 | マージ済み・staging 配備済み |

追加した MCP tools: check_library_exportability / export_app / get_export / prepare_import（ChatGPT file param）/ get_import_validation / import_app / get_import。既存 REST service を呼ぶだけで、新しい import engine や canonicalizer は追加していない。

### 11.2 実アカウント間 E2E（staging、API 経由）

アカウント A・B を staging に作成し、同じ Portable Notes 系静的 fixture（v3）を使って実行した。ログ: .tmp/portable-capsule-20261028/e2e/s1-cross-account.mjs, s1-result.json。

| 項目 | 結果 |
|---|---|
| A: v3 fixture import → validation ready → import-app → verification verified | PASS。app_url 200、receipt schema ato.contract-verification-receipt/1、fully_satisfied=true、target=ato-run-hosted |
| A: app-only export | PASS。出力は v4（ato.portable-application/2）、bundle_sha256 一致、contract_ref は元 K と一致 |
| B: A の export artifact を import | PASS。validation ready、root_contract_ref と derivation_ref が A と一致、新 Instance、receipt fully_satisfied=true、app_url 200、/proof.txt 200 |
| I1 Identity | PASS（root_contract_ref 一致） |
| I2 Closure | PASS（validator ready、digest/size 一致） |
| I3 Independent Authority | PASS（A は B の Instance/export を読めない 404、B は A の bundle を読めない 404） |
| I5 Verified Realization | PASS（両側 receipt fully_satisfied=true） |
| E4 負例（非対応 version） | PASS（prepare が 400 で拒否） |
| E4 負例（改ざん） | PASS（transport digest を再計算しても validator が rejected/validator_failed） |
| I6 Atomic Import | 部分的（idempotency/reconcile は既存 test 済み。実アカウントでの crash 注入は未実施） |

### 11.2b with-data cross-account（PWA ブラウザ export / API import）

静的アプリ（localStorage にノートを保存）で with-data を検証した。A の Export は PWA の App 詳細画面の Export ダイアログから実行し、ブラウザの state flush / snapshot を含む v4 artifact をダウンロードした。B の import は同じ service を API 経由で実行し、B のブラウザで状態を確認した。

| 項目 | 結果 |
|---|---|
| A: 静的アプリにノートを保存 → PWA UI で with-data export | PASS。v4（ato.portable-application/2）、instance_snapshot_ref あり |
| with-data export の K | app-only K とは異なる snapshot-bound K' になる（設計どおり。I4 の「必要なら新しい Snapshot K を seal」） |
| B: artifact import → verified | PASS。root_contract_ref = K'、receipt fully_satisfied=true、app_url 200 |
| B: 状態の再現 | PASS。A のノートが localStorage に復元され、画面に表示 |
| B: 編集 | PASS。追加したノートが 5 秒後の flush 後も保持され、reload 後も残る |
| B: 再起動 | PASS。Instance stop → app_url 再オープン後もノートが残る |
| B: 再 Export | PASS。app-only export が ready、contract_ref = K' |
| I3 独立性 | PASS（A は B の Instance を読めない 404） |

### 11.2c PWA UI import の ERR_ABORTED（原因確定・修正済み）

staging の Add App からの .capsule import で prepare が net::ERR_ABORTED になる件を追跡した。配信中の JS の build id は f5c95587（dev ブランチの merge、stg/main の祖先ではない）で、stg 配備前の一時的な build だった。

原因は 2 つ:
1. /apps/new がアカウント session 解決前にもフォームを描画し、key が signed_out → アカウント id に変わって remount していた。ファイル選択が早いと CreateAppPage の cleanup が in-flight import を abort する（ERR_ABORTED）。
2. Capsule route セレクタの既定が Static Web のため、OCI-only の capsule では候補 0 件になり、原因の分からない portable_selected_derivation_missing になっていた。

修正:
- ato-pwa #558: 単一 realization の capsule を既定 route でも import。複数ある場合は portable_realization_required で候補を示す。
- ato-pwa #559: session 解決まで Add App フォームを描画しない（loading は LoadingView、signed out はサインイン）。
- ato-pwa #560: 追加したサインイン文言を既存カタログの Sign in に置換。

stg を staging に配備（PWA version e2672da9、stg d57e67d2）し、修正後の build で早期のファイル選択（待ちなし）でも prepare 201 → upload → validate → import-app → Ready まで PASS した。

### 11.2d 動的 process + filesystem state の制約

Portable Notes の python process + sqlite volume fixture は、staging hosted runner で path must be shorter than SUN_LEN により verification failed していた（receipt: execution.attempt_interrupted=true, fully_satisfied=false）。

原因は process launch の isolated network が <runtime_root>/egress.sock を直接 bind していたこと（checkout path が長い）。ato #1608 で、native Terminal / ingress connect と同じ /proc/self/fd/<dirfd>/<name> 経由の bind に変更した。

staging hosted runner を 9fbfe8f7 に更新後、Portable Notes の **python process** で import → verification verified、receipt fully_satisfied=true、realization=process、app_url 200 を確認した。OCI と python の両 derivation が staging で成立する。

### 11.2e Portable Notes（OCI derivation + filesystem volume）の with-data

Portable Notes fixture（python process と OCI の 2 derivation、filesystem state）を **OCI derivation** で import し、with-data を検証した。

| 項目 | 結果 |
|---|---|
| A: OCI notes を import | PASS。verification verified、receipt fully_satisfied=true、realization=oci、実行コンテナ起動 |
| A: ノート作成 | PASS（/api/notes 201） |
| A: PWA UI で with-data export | PASS。v4、instance_snapshot_ref あり、約 32 KiB |
| B: artifact import | PASS。verification verified、receipt fully_satisfied=true |
| B: filesystem 状態の再現 | PASS。A のノートが SQLite から復元 |
| B: 追記 | PASS。2 件目のノートが保存 |
| B: 再起動 | PASS。stop → wake 後も 2 件保持（wake は recovery 待ちのため数回リトライ） |
| B: 再 Export | PASS。app-only export 開始（ready） |
| I3 独立性 | PASS（A は B の Instance を 404） |

python process derivation は hosted runner で path must be shorter than SUN_LEN により verification failed のまま（§11.2d）。with-data の Portable Notes 受入は OCI derivation で成立した。

### 11.2f Portable Notes の app-only

Portable Notes（OCI derivation）で app-only の cross-account も確認した。

| 項目 | 結果 |
|---|---|
| A: Notes を import | PASS。verified、fully_satisfied=true |
| A: app-only export | PASS。v4、instance_snapshot_ref なし、K は元 fixture の contract_ref と一致、bundle_sha256 一致 |
| B: import | PASS。verified、root_contract_ref 一致、新 Instance、app 200、notes は空（状態は引き継がない） |
| I3 | PASS（A は B の Instance を 404） |

これで Portable Notes fixture について app-only / with-data / 独立性 / 負例（別 fixture の非対応 version・改ざん）の受入が揃った。

### 11.2g PWA 操作のみの Export → Import → 実行（A の Instance 削除後）

UI だけで以下を実施した（スクリプト: .tmp/portable-capsule-20261028/e2e/s22-ui-only-delete.mjs）。

1. A が PWA の Add App から Portable Notes（OCI）を import（UI）
2. A がノートを作成し、PWA の Export ダイアログから with-data export（UI）
3. A の Instance を stop → Delete data → delete（API。UI の Remove App と同じ service）
4. A の Instance が 404 になったことを確認
5. B が PWA の Add App から A の artifact を import（UI）
6. B の app が 200、A のノートが復元されていることを確認

結果: A の Instance 削除後でも B は artifact だけで同じ K'・状態を再現できた。送信元 Instance の存続に依存しない。

### 11.3 MCP smoke（staging、delegated token）

| 項目 | 結果 |
|---|---|
| tools/list | 46 tools。新規 7 tools を列挙 |
| export_app（delegated token、Origin なし） | PASS。export job が ready になり artifact metadata を返す |
| prepare_import（不正な file URL） | PASS（file_download_failed。認可・経路まで到達） |

### 11.3b MCP 実ファイル import（staging）

192.168. ではなく公開 HTTPS で artifact を配信する必要があるため、ローカルの .capsule を一時 HTTP server + cloudflared quick tunnel で公開し、delegated MCP token で prepare_import から実行した（script: .tmp/portable-capsule-20261028/e2e/s23-mcp-real-file.mjs）。

| 項目 | 結果 |
|---|---|
| prepare_import（tunnel URL, 32 KiB v4 artifact） | PASS。bundle_id と validating を返す |
| get_import_validation | PASS。ready、format_version 4、root_contract_ref と derivation_refs（process / oci）を確認 |
| import_app（OCI derivation） | PASS。import request 受理 |
| get_import | PASS。instance と verified receipt |
| 実行 | PASS。app 200、A のノート（artifact に含まれる状態）を復元 |
| 負例: http URL | file_download_url_invalid |
| 負例: https://127.0.0.1 | file_download_url_invalid |

監査・修正:
- ato-api #938: 取得 URL は HTTPS 必須、userinfo 禁止、localhost/loopback/RFC1918/link-local を拒否、redirect は manual（追従しない）、30 秒 timeout、既存 32 MiB 上限（readBounded）。
- ato-api #939: envelope の開始位置 off-by-one を修正（実 bundle が bundle_index_invalid になっていた）。
- ato-api #940: prepare が返す公開 upload URL の /v1 を内部 Hono 用に除去（upload が 404 になっていた）。
- signed URL の有効期限は HTTP 応答（403/410）として file_download_failed になり、形式検証へは進まない。

### 11.4 未検証・残課題

- PWA UI の import は PASS（§11.2c / §11.2g）。staging は現時点で stg d57e67d2 / PWA version e2672da9。
- MCP 実ファイル import は PASS（§11.3b）。URL は公開 HTTPS である必要があり、期限切れ signed URL はダウンロード失敗として拒否される。SSRF 対策は host 判定 + redirect 禁止までで、DNS rebinding の完全防御は将来課題。
- python process の SUN_LEN は ato #1608 で修正し、staging hosted runner 9fbfe8f7 の実 E2E で PASS（§11.2d）。
- stg の ato lib test は StateAttachmentV1.backing の fixture 不足でコンパイルできず、ネットワーク socket の Linux unit test は host 上で実行できなかった（release build と実 E2E で確認）。
- 19 個の validated capsule_bundles が staging に残存（immutable trigger により削除不可、owner は削除済み。active Instance なし）。

### 11.6 回帰テストの対応（不変条件）

| 不変条件 | 既存テスト / 追加テスト |
|---|---|
| I1 Identity（K 不変） | ato-api src/tests/portable-application-exports.test.ts、portable-instance-snapshot-import.test.ts |
| I2 Closure | ato-api capsule bundle validation の test（bundle/validator 系） |
| I3 Independent Authority | ato-api portable-application-exports / portable-import-recovery の ownership ケース |
| I4 State Preservation | ato-api portable-instance-snapshot.test.ts、portable-instance-snapshot-import.test.ts |
| I5 Verified Realization / receipt 環境 | ato-api portable-verification-ingress.test.ts、runtime launch spec v2/v3 の receipt test |
| I6 Atomic Import / 冪等性 | ato-api portable-import-recovery.test.ts（19 tests）、capsule-import-migration-chain |
| 追加（今回） | ato-pwa portableApplicationClient.test.ts の Capsule route selection 3 tests、ato-api management-mcp-portable-export-import.test.ts（10 tests: scope、実 envelope、redirect、private host、prepare/upload/complete） |

PWA client suite 22 tests、MCP suite 10 tests、tsc は PASS。

### 11.7 Runner rollout（staging）

- 対象: ato-hosted-runner-slot1..6（ubuntu-sugamo）。
- 新 binary: /usr/local/bin/ato-connected-realization-worker-9fbfe8f7（sha256 e8816a3c...）。
- 各 slot に最優先 drop-in zzzzzzzzzzzzzzzzzzzzz-runner-9fbfe8f7.conf を追加し、daemon-reload → restart。全 slot active。
- ロールバック: 旧 drop-in を /home/ekohsuke/runner-dropins-backup-20261028c-slot{1..6}/ に保存済み。新 drop-in を削除して backup を戻し、daemon-reload → restart。
- 注意: slot 再起動で実行中 lease は中断される。切替前に managed workload が無いことを確認した。

## 12. 受入条件

| ID | 内容 | PASS 条件 |
|---|---|---|
| E1 app-only | A Export → B Import | K 一致、object digest 一致、B に新 Application/Instance、K 検証 PASS、A 依存なし |
| E2 with-data | 本文・進捗・画像 Asset | 状態・Asset 一致、B で編集・再起動、再 Export 可能、snapshot K が正しい |
| E3 独立性 | 別アカウント / 別 browser profile / 別 Instance | A の Cookie / Token / Live Instance を使わない |
| E4 負例 | 欠落 object、改ざん、不正権限、Asset 越境、状態更新競合、Import 途中 crash、無効 receipt、非対応 version | 拒否または回復。形式テストと実製品 E2E を混同しない |

## 12. 未対応 / UNVERIFIED

- cross-account の実アカウント E2E（本監査では未実施）。
- validated capsule_bundles の長期保持 / quota eviction。
- v4 import の hosted 経路の実 staging 動作（コードと unit test はある）。
- filesystem volume の with-data 保証（v1 対象外）。
- Export Artifact の MCP 経由ダウンロード方法（tool の返し方）は実装時に確定。MCP は bytes を直接返さず、PWA / 署名 URL へ誘導する案を優先。
