# Formation汎用化 調査台帳

> Status: historical evidence / past plan. Language: ja+en.
> Preserved content; relative navigation updated. Current [documentation map](../../../README.md).

調査日: 2026-10-23。実装の存在、過去の実測、今回の検証を区別する。
この台帳に記載された機能が現在staging/productionで有効とは限らない。

## 調査対象

| 対象 | 調査したorigin/main | 作業前checkout |
|---|---|---|
| ato | `17305afe0820ad7dd1e9c05c99585f837da35227` | nightly `40a4b3548559f189ba30516205b385d5b5fd2672` |
| ato-api / admin | `e10637d7a38ff98162b410f93e81178d12ff29f0` | `4d13be3a49960ac1bed331d34bb355fa6d173289`、既存変更あり |
| ato-pwa | `b4bfa18f` | main `ce5b7913`、未追跡.wranglerあり |

adminはユーザー確認によりato-api内の管理Workerを対象とする。
既存checkoutを切り替えず、workspace `.tmp/formation-generalization-20261023/` に
独立worktreeを作成。アプリがGit repoでないworkspace rootに属するため、
appのcreate_worktreeは `Not a git repository`。git worktreeで代替した。

## 再利用先と不足

| 領域 | 現在の実装・テスト・設計 | 再利用・不足 |
|---|---|---|
| capsule.toml / Preset | `lib/formation/src/{capsule_toml,capsule_toml_v2,preset,authoring}.rs`、`lib/formation/tests/` | 両入口をAuthoringDraft→BoundContract/BoundDerivationへ統一済み。未知fieldを拒否。v1 Intentは互換用。別のmanifest/identityを増やさない |
| 推定 | `detect.rs`, `workspace.rs`, `generation_context.rs`, `proposal.rs` | Python/Node/staticとbounded workspace検出。検出は証拠。任意shell/READMEを実行権限にしない。Docker/Compose/Rust/GPU/GUI等の共通証拠を拡充する |
| Formation | `apps/formation-worker/src/{job,local,runtime_network}.rs`、`lib/runtime-attempt/src/{attempt,executor,formation_realizer}.rs`、`request.rs`, `search.rs` | build/launch/verify、attempt/round/deadline/予算/fenceを再利用。別の自動retryループを作らない |
| Static / Process / OCI | `projection.rs`, `source_oci_plan.rs`, `apps/portable-application/src/`, `extensions/adapters/{process,oci}` | 固定image、read-only workspace、runtime admissionとbundle検証の分離を維持 |
| 実行要件 | `requirements.rs` のExecutionRequirements/HostRequirement、Dのsteps/ports/state/variables/runtimes | 新しい意味的rootやテーブルは不要。CPUは既存OCI limits、RAM/VRAM/diskはhost要件。推定情報をDへ自動挿入しない |
| Managed / User Runner | API `runner_provisions.ts`, `compute_connections.ts`, `runner_leases.ts`、Rust connected-realization-worker | enrollment、Execution Identity、Lease、Heartbeat、recovery、owner認可を共用 |
| GPU | API `provider_adapter.ts`, `providers/runpod_provider.ts`, `runner_provisions.ts`、`runpod-provider.test.ts`等 | RunPodのみ実Adapterあり。Vastは未実装。create結果不明時の照合、削除確認、scope付き孤児回収を維持。Providerを跨ぐRun単位の予算予約は不足 |
| 料金 | `compute_provision_status.ts`, `compute_connections.ts`、Draft API #816 / PWA #491 | 現行推計と請求額を分離。単一Pod上限と全Provider累計$3は別。Draftのstorage対応を既存配備済みと扱わない |
| Native Surface | `lib/ipc/src/{session_surface,terminal_surface}.rs`、`services/netd/src/{pixel_gateway,terminal_gateway,surface_authorization}.rs` | Web/PixelStream/Terminal契約、認証済みWS、RFB初回frame/PTY probeあり。submissionはWeb/Terminalのみ。GUI producer、resize/file/clipboardは追加契約が必要 |
| PWA frame | `src/components/runs/{SessionSurfaceRenderer,noVncPixelStreamTransport,xtermTerminalTransport,TerminalSurfaceConsole}.*`と同名test | 同じframeの3経路を再利用。noVNC v1は固定viewport、clipboard/fileは無効。これを既に全対応と報告しない |
| Snapshot / K / Receipt | `lib/formation/src/{verify,receipt,retained}.rs`, `apps/portable-application/src/instance_snapshot.rs`, `tools/receipt-authority` | SnapshotはMaterialization。Surface readyはK成立ではない。Rust authorityだけがfully_satisfiedを決定 |
| 複数サービス | `capsule_toml_v2.rs`, `lib/ipc/src/oci_service_group.rs`, `PORTABLE_OCI_SERVICE_GROUP.md` | 2〜4 OCIサービス、既存BoundStep/Port、内部network、作者順起動・逆順停止、全体失敗あり。DAG・複数HTTP観測・process混在は未対応 |
| Secrets / Network / Storage | API `secret_crypto.ts`, `ai_gateway.ts`, `runtime_network/variables.ts`, `data_resources/bindings.ts`, `network_authorization.ts`、Rust `variables.rs` | AES-GCM/AAD、BYOK、Run単位proxy/予算、state/grantを再利用。Public Preview用の固定用途Brokerとアプリallowlistは別に必要。任意promptを扱う既存AI gatewayをそのまま公開しない |
| Discover / Library / Publish | API `discover_apps.ts`, `publish.ts`, `compute_instances/{forks,static_definition}.ts`, `portable_application_imports`、PWA personal-apps | 既存登録/公開/個別Instance生成を維持。実行成功とpublication成功を別々に記録 |
| admin | API `admin_worker.ts`, `wrangler.admin.toml`, `routes/admin*.ts*` | 既存admin認証・step-upを再利用。管理権限を利用者データ権限へ拡張しない |

## 関連PR・Issue

2026-10-23に接続済みGitHubで確認。以下は既存PRであり、今回作成したPRではない。

- [GPU host要件 #1473](https://github.com/ato-run/ato/pull/1473)
- [Model SetをDへ結合 #1482](https://github.com/ato-run/ato/pull/1482)
- [共通model delivery #1493](https://github.com/ato-run/ato/pull/1493)
- [Runner provisioner設計 API #724](https://github.com/ato-run/ato-api/pull/724)
- [bounded RunPod API #760](https://github.com/ato-run/ato-api/pull/760)
- [GPU+storage予算 Draft API #816](https://github.com/ato-run/ato-api/pull/816)
- [同予算表示 Draft PWA #491](https://github.com/ato-run/ato-pwa/pull/491)
- [seccomp Draft ato #1500](https://github.com/ato-run/ato/pull/1500)、[受理schema API #819](https://github.com/ato-run/ato-api/pull/819)
- [Wan modes Draft ato #1501](https://github.com/ato-run/ato/pull/1501)、[API #817](https://github.com/ato-run/ato-api/pull/817)、[PWA #492](https://github.com/ato-run/ato-pwa/pull/492)
- [実機GPU試験 API #763](https://github.com/ato-run/ato-api/pull/763) と [Go/No-Go Draft #818](https://github.com/ato-run/ato-api/pull/818)
- [Formation outcome設計 #1391](https://github.com/ato-run/ato/pull/1391)、[共通attempt #1390](https://github.com/ato-run/ato/pull/1390)
- 関連tracking: [API #630](https://github.com/ato-run/ato-api/issues/630)、
  [#634](https://github.com/ato-run/ato-api/issues/634)、
  [#635](https://github.com/ato-run/ato-api/issues/635)、
  [#636](https://github.com/ato-run/ato-api/issues/636)。本文とopen状態を個別確認。
  それぞれ動的アプリ基盤、休止抑止、復旧時の外部作用抑止、Private/egress境界を扱う。
  9月時点の背景を含むため、openという状態だけから現在の実装不足を断定しない。

## OmniVoice独立コピー

過去証跡は元workspaceの `.tmp/oss-five-fixes/operations/omnivoice/` にある。
`first-use-ui.json` は共有Instanceの音声生成・WAV再生を記録する一方、
`fork-copy-failure.json` は separate_copy受理後の `compute_instance_artifact_missing` を記録。

現行 `compute_instances/forks.ts` はCompute/Schema/Instance、kind、process realizationを
コピーするが、portable_application_importsのBundle/選択D/Workspaceの関連を作成しない。
app_proxyはportable importを見つけられず、Static Web resolverへ進む。
動的portable appにはstatic artifactがないため、この独立コピーが欠落する。
修正は既存portable importの新規Instance作成経路の再利用が必要であり、
static artifactを捏造したり、元のLease/秘密値/利用者data/receiptをコピーしてはいけない。

別の起動試行にはdata_plane_unsupported、Unix socket長、HTTP502の証跡がある。
これらを独立コピー失敗の同じ原因へまとめない。過去証跡は今回の再現試験ではない。

## PR分割・検証gate

1. D/ExecutionRequirementsを維持した証拠検出とread-only解析。旧digest、symlink、容量、秘密値非出力を検証。
2. 既存Native Surfaceのproducer/接続制御。固定デスクトップ公開は追加しない。readyとKを独立計測。
3. 既存service groupに依存解決・全体K観測を拡張。旧single-serviceのdigestを固定。
4. 既存Secret/Grant/Gatewayを基礎に管理allowlistと固定用途Broker。任意request/prompt環境は拒否。
5. ProviderAdapterへVast、共通予算予約、同一Runの排他、削除照合。実GPU作成は別途承認。
6. 既存bounded searchへの統合、Library/Discover/再接続/保存のstaging回帰と計測。

全6項目の完了には実装と実環境検証が必要。unit/mock成功をstaging実測へ置き換えない。
本番deploy、課金GPU作成、運営キー本番投入は未承認で、実施しない。

## 今回の実装と検証

以下はすべてDraft。意味的なK/D/wire、DB table、migration、flagは追加・変更していない。
新しいinspectionは表示専用であり、正規化済みDの代替・実行認可には使わない。

| PR | 実装した範囲 | 残る範囲 |
|---|---|---|
| [ato #1502](https://github.com/ato-run/ato/pull/1502) | 既存D/Kを借用する要件表示、bounded repository証拠検出、`form-inspect` | Git URLの取得からFormation実行までの自動統合、Compose解決、実測への昇格 |
| [PWA #506](https://github.com/ato-run/ato-pwa/pull/506) | GUI/PTYの共通接続寿命、同じSurfaceへの手動再接続、古い状態/DOM書込の排除 | GUI producer、転送/clipboard/GUI resize/idle、実RFB/PTYのstaging検証 |
| [ato #1504](https://github.com/ato-run/ato/pull/1504) | `depends_on`を既存の順次起動へ安定topological sort。重複/未宣言/自己参照/循環を拒否 | 複数の個別HTTP・接続・全体操作のK観測、異種runtime混在 |
| [API #831](https://github.com/ato-run/ato-api/pull/831) | owner/attemptに結び付く永続code/stageから失敗分類・retryabilityをpoll/waitへ同じ形式で表示 | retryの自動実行、GPU予約/Provider切替、Managed Secrets Broker |
| [ato #1505](https://github.com/ato-run/ato/pull/1505) | Linux限定OCI explorationの必須userフィールドを補完し、image USERを継承 | 実OCIコンテナの起動検証 |

ローカル検証:

- #1502 source `841cd0b17`: ato-formation 447 tests、CLIの新規2 tests、
  `cargo check -p ato-cli`、workspace全target/feature clippy、fmt PASS。
- workspace全テストは1,985 PASS / 1 FAILED / 9 ignored。残る1件は
  `deny_bridge_tests`がcrate内の`.tmp`へ固定生成するUnix socketの長さ超過。
  短いworkspace内TMPDIRだけではこの相対パスを置換できない。同じテストバイナリの
  該当1件を短いworkspace内cwdから実行するとPASS。全suite成功とは報告しない。
- #1504 source `1e0e5d803`: ato-formation 441 testsと同crate clippy PASS。
- APIの単体16 tests、実Workers/D1 outcome 12 tests、全体typecheck PASS。
  Workers suiteは既存projectのhook 120秒/test 30秒の一時configを使う。
  既定hook 10秒での成功や、mockの実機成功への読み替えは行わない。
- PWAの既存noVNC/xterm/共通frame 21 testsとfull build（型検査/i18n/staticを含む）PASS。
- 実OmniVoice sourceにfresh CLI環境から`form-inspect`を実行。Python宣言、
  Web/GPU/modelのhint、4つのentrypoint名を検出。`runtime_verified=false`。
  この検査ではアプリをbuild/launchしない。

CIではAPI/PWAのannotationに課金制限・開始stepなしを確認した。コード失敗と扱わず、
課金設定変更や再実行は要求しない。一方RustのLinux E0063（OCI user欠落）と
macOSの`http_gate_forwards_binary_body_byte_for_byte`失敗は実行済みstepのエラーであり、
課金例外へ含めない。前者は#1505に分離。後者は原因未確定として残す。
macOSからのLinux cross-checkは`x86_64-linux-gnu-gcc`不足で到達できず、
Linux検証成功とは数えない。
修正#1505 `5ace78f76`ではUbuntu CIのcheck/test/clippyがPASS。
同PRのWindows suiteはloopback CDP待機timeoutで1件失敗しており、全OS成功ではない。

共有stagingの調整メッセージ送信はユーザーが明示許可済み。
「OSS 7件のFormationとstaging掲載」は切替可能、保持対象はHarperの承認済みsource、
Formation/Receipt/curation記録とInstance。掲載不可状態を維持する。
「公開前の確認と改善を完了する」も切替窓と最終修正`73e8dd87`を記録した。
既存配備のAPI `320fb0105a49` とPWA `f4dd42458d54`を基に、今回の差分と同修正を
追加した専用integration worktreeを準備した。既存checkoutの切替・上書きはない。
integrationのAPI Library/Discover/outcome 100 tests、分類16 tests、全体typecheck、
PWA Library/Discover/Surface 188 tests・staging full buildがPASS。

APIは`82b22d266767` / Worker `9f1f9eb5-047a-4668-89ad-bd9fccdf991e` 100%、
PWAは`f58babe61a00` / Worker `4368c545-2323-426e-b99f-97ec26abb597` 100%へ直列配備。
通常API configは存在しないExecutorを参照し、最初のuploadをCloudflare10143で拒否。
直前のbinding整理`e8fe4bcf`の現設定を保全する専用configをintegrationにcommitし、
既存safe package scriptで配備した。全120 binding metadataが前後同一、Secret37保持、
migration pending0/適用なし、provision flags1/1/1/live provisions0を維持。
PWAのmain/CSS/Native Surface lazy chunkの配信bytesがlocal buildと一致。
配信確認の単発値はshell200/366ms、API health200/280msであり、アプリ起動性能ではない。
ChromeでDiscover/既存Libraryの読み取り表示・console error0を確認。Run作成はなし。
実RFB/PTY、保存・復元・再接続、複合K、GPU実行は未検証。
共有profileはcanonical lifecycle skillのstaging専用profile条件を満たさず、同suiteは
未開始。読み取りsmokeをlifecycle受入として数えない。
2026-10-23T14:12:15Zに共有stagingを解放し、両チャットへ最終版を通知した。

Managed Secrets、Vast、Providerを跨ぐ予算予約/排他、全体自動化はまだ実装していない。
この台帳および上記PRをもって、依頼全体の完了・GPUの稼働・運営キーの安全な提供・
既存アプリの保存/再接続のstaging回帰なしを主張しない。

## 2026-10-24 続行時点の差分台帳

上記「今回の実装と検証」と末尾の未実装記述は初回5 PR・初回stagingの記録。
以下で更新する。Draft実装の存在、隔離CPU実測、共有staging配備、機能有効化を分ける。

| 範囲 | 追加Draftと確認した実装 | 受入の残り |
|---|---|---|
| GUI/TUI producer・transport | ato #1506/#1507/#1509/#1511/#1512/#1513。private Xvnc、PTY、Run/Lease/Runner scoped assertion、owner EOF/idle cleanup | Hosted Runnerとの一体所有、保存/復旧、製品GUI viewport変更・ファイル/clipboard |
| Native観測・wire | ato #1514/#1515/#1516/#1518、API #842/#848。Dのoptional Native catalog、bounded RFB full-frame測定、入力を再実行しないprivate HTTP観測、selected Dを含むlaunch digest | Native実行guardとcapability無広告を維持。製品公開/実Hosted K Receiptは未達 |
| 複合K | ato #1508、API #838。全logical Port観測、owned container handles、private observation proof、全観測のRust判定 | 共有Hosted配備と再接続/保存の受入。異種runtime混在は未対応 |
| Managed Secrets | API #837/#841/#843/#844/#846。既存暗号化/Grant、固定text-embedding Broker、named Admin entrypoint、用途別Admin認可、safe additive deploy | Brokerはstaging無効、policy/key未投入。実workload/egress受入、任意chat/URLの運営キー提供は不可 |
| GPU | API #836/#839/#840。共通予約、Vast Adapter、同Run排他、費用内訳・期限・停止後削除/孤児照合 | 実Provider/GPU試験は未実施、課金作成は承認直前で停止する。Vast flagは無効 |
| 推定・転送 | ato #1510/#1517。既存ProposalV2へ証拠検出、bounded file-backed GitHub取得、redirect/decoded amplification拒否 | 通常Formation jobとの全体統合、初回GitHub archive承認→実行 |
| 失敗・再試行 | API #845/#849。保存code/stageと既存Search予算の投影、既存fence/created_atによる3 claims/1h、単一D1 batch/CAS | 新しい自動再試行schedulerは作らない。期限の自動kill/未接触job expireは今回のclaim受付制限に含まない |
| GitHub入口・証拠 | API #847/#850。pinned commit/subdir、既存from-source、Job/attempt/ownerへsource receiptを束縛、明示archive承認 | 旧wizard receiptの実測不足は409拒否。現Formation実測のreceiver/実archive保存を実装中 |
| UI成功判定 | PWA #508。Add/Updateがready/succeeded＋Schema＋失敗なしの場合だけ進む共通判定 | 未配備。失敗/UNKNOWNにstale Schema IDがあっても進まない65 fixtures/full build確認 |

Native組み立ては `apps/native-surface-gateway` に置く。netdのservice層から
Formation libへ依存した初回CI違反は修正し、既存architecture ruleは変更していない。
同名共通binary、既存transport、shared catalogを再利用する。Linux app+netd全target
119 tests、macOS新app9 tests、all-target clippy、architecture44 packagesが通過。

隔離CPUの実Agent PTY＋probe＋Surface準備は207ms、実TigerVNC＋probeは977ms。
複合OCIのgood caseは23,275msで全K satisfied、backend失敗caseは23,545msで
frontend200でも全体false、両container削除を確認した。単発sampleであり共有stagingの
起動性能ではない。証跡は各PRのdocs/opsとevidenceを参照する。

NativeのRust/TypeScript shared golden bytes/digestは一致し、旧v1/v2/v3 fixturesも維持。
CLIの明示failure Receipt exportに対する旧「Receipt不在」期待はCIで検出した。
更新テストは非ゼロ終了、fully_satisfied=false、同じK/D、failed observation、停止した
endpointとfinished failed attemptを検証する。Mac portable CLI全16 testsが通過した。

共有stagingはphase4でAPI `c54fe31e-55d5-40ce-9a9b-9fff83d117a7`、Admin
`1677264a-a7fb-48bd-ae4e-346b608bbc89`。既存117 API bindings/settingsを全保持し、
用途別の非secret Admin membershipだけ追加（118 total/36 secrets）。実Chromeの既存
Access/sessionでAdmin GETが通り、無効Brokerのnot_found404を確認した。API public
admin routeも404、Broker無効、policies0/liveGPU0、Vast未有効。D1 read-only snapshotsは
Harper source/Instance/Schema/Receipt/curation/未掲載状態を保持。phase4 lockは解放済み。
続くAPI #845/#849/#847/#850/#848統合は242 tests/typecheck通過、まだ未配備。

### 追加で検出したauthority不足

APIが参照する旧 `crates/snapshot/src/source_receipt.rs` / source-materialization
producerはmonolith dismantle commit `4a5df3bc7`（2026-08-14）で削除済みで、現行Rustに
同じproducerは見つからなかった。APIのqueue/claim/reportは残っているが、これを
稼働中のproducerとして扱わない。旧 `ato.capsule-program-source-projection/v1` と現行
`ato.source-resolver.v1/v2` は別authority。raw codeloadを `ato.source-archive/v1` へ偽装せず、
実測・保存証拠がないmaterializationはnull/実行拒否のままにする。現Formationの既存
source_evidence経路を使うreceiver-firstの修正と実archive保存を後続で進める。

本番配備、課金GPU作成、運営キーの本番投入は実施していない。全体完了・全種のHosted
実行・運営キーの実workload安全性・既存アプリの保存/再接続回帰完了とは報告しない。


## 2026-10-24 phase16 更新

前節の「未実装」「未配備」は各節の記録時点を表す。現在の差分は下表で更新する。
Draftコードの存在、隔離Runtime実測、staging配信、Hostedの機能有効化を区別する。

| 範囲 | 追加Draft / 再利用した境界 | 現時点の残り |
|---|---|---|
| Native実Run所有・Files | ato #1519以降の既存HostBoundary/Run Output連携、#1536。APIの既存owner/data grant admission | 実Hostedの保存・復旧・他利用者分離を同じattemptで確認 |
| Model / 複合入力 | ato #1535/#1537、API #873/#874。既存DeliveredInput、readonly ModelSet/Asset、component input_idsとOutput scope | 実GPUモデル・Hosted複合Run。異種runtimeを跨ぐ単一複合Dは未対応 |
| Source全体連携 | APIのarchive/measurement/owner source transfer、ato #1538、PWA #515。既存Formation jobと固定source pointerから既存Satisfyへ接続 | 認証済みownerから実GitHub job→選択D→Hosted receipt永続化 |
| GUI貼り付け | ato #1539。既存assertionの既定false権限、全packet検証後のLatin-1転送、fragment/expiry/共有頻度制限 | 実Chrome共同試験進行中。Hostedは未有効 |
| GUIリサイズ | ato #1540。既存gateway/署名、bounded SetDesktopSize、元K固定、実TigerVNCのACK/fullframe/reconnect | 実Chrome共同試験進行中。Hostedは未有効 |
| noVNC互換 | ato #1541。既存bounded SetEncodingsからExtendedClipboardだけを除去、negative CutText拒否維持 | 統合Linuxの同一source検証とChrome共同試験 |
| trusted helper | ato #1542。検証済みOCI ABI label→既存helperの固定argv、物理controls→既存Ready/heartbeat、default off | shared Runnerへ未インストール・未広告 |
| Native操作認可 | API #875/#876/#877。既存HMAC、Ready extension、owner/live lease/accepted receipt/現行能力の積、app-origin preflight | stagingコード配備済み。認証済みowner/browserと対応Runnerの受入 |
| 共通アプリフレーム | PWA #517。既存renderer/noVNC公開API、明示Paste、サイズ変更、古い非同期結果の破棄 | staging資産配信確認済み。実Chrome fixture作成中、Hostedは未検証 |
| Managed Secrets | 既存固定Embedding brokerのSDK/network/relay/用途別Admin拡張 | staging broker無効・policy0。実上流/運営key試験なし。任意認証request/自由prompt/URLへ運営keyを渡せない |
| GPU共通予算 | RunPod/Vast共通予約・Run排他・削除/在庫/孤児照合はDraft実装済み | 実Provider未検証。既存31未予約intentの保守的予算計上によりclosed、Vast設定なし。課金作成直前の別承認が必要 |

phase16の共有stagingはAPI source64d50b0b / active100%77f0b0b9、PWA sourceb065887a /
active100%23a4c432、Admin271d6291変更なし。両チャットの切替可能回答後にlock下で
API→PWAを直列配備し、独立監査で119 bindings/36 Secret参照・設定保持、6 readonly D1
結果前後一致/書込0を確認した。未適用migrationなし、適用なし。実HTML200と到達可能な
主要5資産がlocal build bytes一致、staging API origin/Native controls/Files/Runtime markerを
確認。stage16 ops/proofは各所有repoに保存し、lockを解放して両チャットへ通知済み。

API #877は175 distinct tests/7 suites、全体typecheck、PWA #517は53 tests/6 suitesと
型検査/full buildが成功。統合PWAは追加78 tests/7 suites成功。API #876の204 tests、
#875の48 testsは重複を含むので合算しない。PWA広範囲の303成功/SessionTrayコピー期待値
1失敗は同一baseでも再現し、スキップを追加していない。

隔離Linuxでは#1540の実TigerVNC1024×768/fullframeと#1542の通常/root UID65534における
paste/resize/reconnect/元K/stop-save/回収を確認した。#1542はMac440/Linux441 testsと
Clippy成功。これはlocal Coordinator/canary認証のfixtureで、staging Hosted/本物の運営key/
実Providerの受入ではない。GUI起動deadlineの一度の未解明失敗と、その後の同一deadline
fresh-root成功は各PRのopsへ両方保持している。

手動staging専用Chromeログインは既存質問の回答待ちで、認証の迂回はしない。
本番配備・課金GPU作成・運営key本番投入は行っていない。依頼全体の完了、全種のHosted
実行、既存アプリの保存/再接続回帰完了とは報告しない。

## 2026-10-24 phase17 更新

GUIの実Chrome結合は[ato #1543](https://github.com/ato-run/ato/pull/1543)と
[PWA #518](https://github.com/ato-run/ato-pwa/pull/518)で成功した。
接続中の実Xselection一致を必須とし、resize1024×768、BrowserとRust各再接続、権限拒否、
元K、固定probe、出力保存、全回収を確認。切断後None・未知null・全体ANDを分離した。
helper6902ms/lease11363ms/Browser8.6s/Root全体21.6sは隔離CPU実測で、Hosted性能ではない。
初回2失敗と修正・negative拒否を保持し、通常Runner候補のprepare-only検証へ進める。
詳細はnative-browser-integration-20261024.md/proof JSON。共有Runner6slotは未変更、
Hosted owner認証・実Source job・専用Runner導入・GPU/運営key実受入は依然別gateである。

## 2026-10-24 phase18–21 更新

| 範囲 | 追加Draftと検証 | 現時点の残り |
|---|---|---|
| 明示Runner配置 | [ato #1544](https://github.com/ato-run/ato/pull/1544)、[API #878](https://github.com/ato-run/ato-api/pull/878)。既定offの既存capability markerとLeaseで自動選択除外・明示配置・claim CAS・実行前照合を強制。K/D/spec/digestに配置を混ぜない | 新identity/実machine/owner認証後に導入。共有Runnerは未変更。旧pending先頭の不正Leaseは後続を止め得るため、新identityは空queueが前提 |
| TUI共通フレーム | [ato #1545](https://github.com/ato-run/ato/pull/1545)、[PWA #519](https://github.com/ato-run/ato-pwa/pull/519)。実Chrome keyboard→実PTY120×40、同Surface再接続counter2→3、同PID、固定K、保存、全回収 | fake Coordinator認証の隔離CPU試験。認証済みHosted/実OSSの受入は未達 |
| test runner境界 | PWA #519でNode guardがVitestに誤収集される実不具合を修正。内容不変renameとNode22 CI step、Node11/skip0・Vitest39/4suite成功 | 元の2失敗ログを保持。最新CI成功は未確認。ブラウザ実行6ファイルはbyte不変 |

明示配置はRust Mac189 tests、API226 focused tests/typecheckと独立reviewが成功。
API生成のv1/v2/v3 JSONを凍結Linux receiverで直接検証し、明示ON3件受入・必須証明なし
OFF2件拒否の5期待が一致した。これはworkloadを起動しないcross-wire試験である。
先行MacではTerminalのLinux OCI条件により1件が拒否され、元の失敗記録を保持した。

TUIの実行PWA sourceはa6ea2478、Rust sourceはb69b7cc6b。Mac192/Linux193 tests、
all-target Clippy/fmtが成功。Browser case6.6s/lease8437ms/Rust8.77s/Root全体11.482sは
隔離試験の単発値であり、Hosted起動性能ではない。Stopはbounded forced137である。
同sourceの12秒abort負例は1.653秒でfailedとなり、未知null・元K/保存・全回収を維持した。
初回Ready前のtest-only前提誤流用による失敗も保持している。詳細は
native-terminal-browser-joint-20261024.mdとPWA側acceptance記録。

phase20ではAPI source11d75a2cをsafe deployし、active100%
3f7e0c43-2c86-4d2c-abfd-14ff5a9e62cbを確認した。119bindings/36Secret参照とruntimeを
保持し、7つのSELECT rowset前後一致・各書込0・Harper既存状態/liveGPU0/policy0/
明示marker Runner0を独立監査した。Admin271d6291、PWA23a4c432は変更なし。
未適用migration0、適用なし。health200/262msと未認証6経路401/55–382msは
HTTP境界の確認であり、認証済み受入や起動時間ではない。最初のD1 --fileが返した
import transport metadataを保持し、個別結果はSELECT-only --commandで採取し直した。
非version設定の前後検査はsafe script内部で実施、独立before snapshotはない。
共有lockは解放し、許可済み両チャットへ配備結果を通知した。

通常Linux Workerは同source b69b7cc6bのdev/debug0候補50,490,512 bytesをprepare済み。
インストール・Enrollment・capability広告・flag変更・実owner Runは行っていない。
PWA #519はtest/CI/opsだけのため、製品資産が配備済みb065887aと差分0であることを確認し、
追加配備していない。API6 jobとPWA旧SHA2 jobは支払い/spending limitでstep0未開始。
ローカル収集不具合とは別に記録し、CI成功扱いや手動再実行はしない。

残る受入条件は、手動のstaging専用Chrome owner認証、1owner/1Runの実machine境界、
Source job→選択D→同attemptのRust receipt永続化、Hosted保存/復旧・多利用者分離、
実Providerと運営key workloadである。既存Chrome接続はprofile使用中エラーで、共有browserを
停止したりcookieを取得したりしていない。専用VMは未準備、KVM/Firecrackerの存在だけで
成立したと扱わない。異種runtime混在、自由prompt/URL向け運営keyは未対応。
GPUは既存31未予約intentの保守的9300c計上に対する300c上限でclosedであり、実請求額ではない。
Vast設定なし、Broker無効/policy0。本番・課金GPU作成・運営key本番投入は未実施で、
現時点で承認待ちの作成要求はない。
