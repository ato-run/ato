# Formation汎用化 調査台帳

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
