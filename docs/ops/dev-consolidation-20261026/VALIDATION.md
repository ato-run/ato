# dev統合・staging検証結果（2026-10-26）

devへの統合、95 PRの包含確認とClose、staging検証を実施した。全完了条件は未達。特にSource Upload期限後の永続Archive再利用はP0として残る。PR差分の統合と、capsule/2やStateライフサイクルの機能完成を区別する。

本作業によるProductionデプロイ、mainへのpush、admin bypass、force push、既存branch/worktree削除は0。mainの読取snapshotには別セッションの更新があるため、全セッションを通じたmain/Production不変性は主張しない。

## dev統合

| Repo | 実装検証・配備対象のdev SHA | 統合 |
|---|---|---|
| ato | `bff38ed818858c99c153e66bfc3f77b1f88289ff` | #1576、#1580。#1575のGate A、#1531の残文書、CLI失敗Receipt回帰を吸収。別セッション#1579のK/Dヘッダー・publication修正も包含 |
| ato-api | `16d1c53da0c7c4f53ca6989cc6981a58fedbec06` | #890、#892、#894。#889のcapsule/2受理、Digest別Source承認UI、bootstrap同期、同一bundleの所有者内重複処理 |
| ato-pwa | `33febac1717bb4c8a5d66e4ea87e5d12ae4d27c1` | 本作業でコード変更なし。進行中の別セッションのdev更新を保持し、最新配信・typecheck/build/Vitestを確認 |
| ato-contents | `main d13cb5f…`、devなし | 15 branch headはmainに包含。変更なし |

共通のcapsule_toml_v2 / Bundle Builder / Rust verifierを再利用した。CLIのportable executorは共通crateへ移り、TypeScriptへcanonical KやContract判定を再実装していない。既存devに包含済みのNative/Formation/Sourceコードを重ねてCherry-pickしていない。変更ファイルとMigrationは[CHANGED-FILES.md](CHANGED-FILES.md)。

Source承認画面は既存の管理API・step-up・CSP/CSRF境界へ接続した。ユーザーの指示に従い、Gmailの対象確認メールを読み、コードを画面へ入力し、下記2つの合成Archiveを承認した。コード・Cookie・Tokenは成果物へ保存していない。

## 必須検証

| 対象 | 結果 | 条件・制限 |
|---|---|---|
| Rust Linux Workspace | PASS | 追加修正込みbff38ed81、cargo check --workspace --all-targets --locked、cargo test --workspace --no-fail-fast --locked、cargo fmt --all -- --check、cargo clippy --workspace --all-targets --all-features --locked -- -D warningsが全て終了0 |
| Rust test集計 | PASS | 各harness出力の単純合計2408 PASS / 0 FAIL / 37既存ignored。子harnessを含むためunique test数とは扱わない。skip追加なし |
| macOS Runtime | PASS | symlink化TMPDIRをcanonicalな親に直したfixtureで197 PASS / 0 FAIL。symlink拒否などの保護は維持 |
| Frozen K / Scenario / Source安全性 | PASS（Linux suite） | 候補によるK再定義、Scenario失敗、Digest・traversal・symlink・展開上限の正負例を既存全Workspace内で検証。実機HTTPの検証とは区別 |
| API | PASS（関連群） | typecheck、capsule/2/Source UI/認証等315 tests、最新関連4 files 65 tests、重複修正8 testsが成功。重複を含むため合算しない。全API test群の成功とは主張しない |
| API schema | PASS | schema:generate / schema:check。pnpm10 frozen installも成功。lockfileはoverride宣言のmetadataを同期し依存解決を変更していない |
| PWA | typecheck/build PASS、Vitest FAIL | 最新33febac1: 333 files PASS / 8 FAIL、3085 tests PASS / 11 FAIL。失敗11件の名前集合はmain 09462869の比較実行と完全一致。#525へ継続。テスト削除・skipで隠していない |
| GitHub CI | 一部FAIL/未実行 | API #890は課金上限による実行前失敗。Linux実機PASSと区別。RustのWindows異常終了とUbuntu Scenarioの非再現失敗は#1578へ。CI全成功とは報告しない |

## staging配信

| Component | 実commit / version | 確認 |
|---|---|---|
| API | `16d1c53d` / `6dab6a21-1521-4aef-8ce4-3b20b059c51a` | safe deploy script、active version一致、36 secrets保持、health200 |
| Admin | `6e178c90` / `2fd15741-7c30-4c13-b61b-549d54f8d454` | safe deploy script、active version一致、3 secrets保持 |
| PWA | `33febac1` / `c37203b4-dd6a-4b30-a3b0-966a36121eef` | 別セッションの配備を確認。実配信index-2mT3TY-t.js、SHA256 289dbf01edf0968c3af4ba7f60461fdd7fafcc1f2b3c34f76a275103b45b307f、staging API originとcommit marker一致 |
| Formation Worker 2 units | `4d43ad44`、clean、build_id=dev-combined-4d43ad44 | immutable candidateへ更新。ato-formation-worker PID3515303、managed-stg PID3515331。旧候補・他セッションのdrop-inは保持 |
| Explicit Linux Runner | `dc5c110a` | Native/State実機検証の対象。後刻別セッションが同じbinaryをwrapper経由で再起動。実processとmain wrapperを区別 |
| Shared managed Runner | `0fabc1f7e`（既存6 slots） | Process ABIを使うcapsule/2の2 Runはこのpoolで実行しReceiptを取得。既存稼働workloadは再起動していない |

stagingのみ `0363_formation_portable_schema_kind.sql` を適用。process17 / static_web488を保全。別セッションの `0363_discover_moderation.sql` と同一番号だが別ファイルで適用済み。適用済みmigrationの改名・再適用はしていない。Production D1への操作は読取のみ。

portable laneは一時primary/allowlistとし、本作業の検証は元からallowlistにいたアカウントで実行した。終了後、portableのlanes/primaryScopeを削除し既定OFFへ復元・readback一致を確認。途中で別セッションがaccountAllowlistを2→3へ変更したため、その無関係な更新を保持した。static/python等を上書きしていない。

## E2E判定

| 試験 | 結果 | 実機証跡・未達条件 |
|---|---|---|
| A Native Terminal | PASS（最終配信後の再試行も成功） | 14 checks: 起動/IO、120×40→100×30、切断再接続、同一PID/starttime、counter継続、Stop、Cross-user Run/Surface/Runner拒否。稼働中gatewayでevil Origin403、偽Assertion401。最終API/PWA反映後の初回再試行は共有slot使用で409。解放を確認後の再試行でも全14 checks PASS |
| B Formation | PASS（対応済みv1/v2経路）/機能完成は未達 | v1回帰、v2 Source、Frozen K、実HTTP verifier、Formation Receipt、同一ArtifactからHosted RunとRunに束縛したReceipt。capsule/3、HTTP body破壊、Node20.20.2 Policyは拒否・success resultなし。Candidate K再定義はRust suiteで確認。全構文/全runtime対応、Receipt条件#1582は未完 |
| C Source Archive | PARTIAL / 長期再利用FAIL | Upload・保存・Digest・取得・同一Source再Formation・Revision・Old/New・旧Receipt不変・Cross-user拒否はPASS。期限後の元Archive参照から再Formationは410 upload_expired。#895をP0とし、表示取得だけで成功扱いにしない |
| D ライフサイクル | PARTIAL | 合成Source→Hosted Formation→Contract verification→Hosted Process Runを同一Source/K/D/bundle/Runで追跡。Terminal再接続・同一Instanceのcounter state restoreは別fixtureで成功。memosの単一OCI wakeと同一InstanceでのD更新をこの結果で解決済みにしない（#1571/#1572） |

新しいbundleを登録した直後、非同期validatorがpendingのためAPIが409拒否し、その後bundleがreadyになる競合を実際に再現した。#1580は同一結果・同一fenceでこの409だけを最大60秒待つ。別の拒否・stale fenceは即時終了する。修正後の新規Artifactは2件とも成功したが、その2回で待機branchが実際に発動したことまではログにない。待機・予算・fence維持はunitで検証済み。

固定8081を直接bindするProcess DはFormationで成功したがHostedでPermissionErrorとなった。Sandboxを緩和せず、既存ABI `ATO_ENDPOINT_APP_HTTP_PORT`を読むDに変更すると同じKで成功した。この環境差も#1582へ残した。

## Source / K / D / Receipt / Runの対応

| 検証 | Archive digest | K | D | Job / bundle / Run |
|---|---|---|---|
| v2 Source1 | `sha256:4f6e211cac1c3cb33de03a8cda690122619cd18e5cd3607fd3d6a6e2daed9d72` | `sha256:884ed35a6c7abe1a9daa4078d8371309ed3332a321fa75061cb43987264c3667` | `sha256:508def413d8266f10d24d3bc813e5e57e3a7e203a2175e82321b8bf67b681541` | Job `fjob_6e1e066aa5b9e6b6ebf95ee93cb5d3466dbe928510be43d0b7f7bbe2f4eaee8b` / bundle `bnd_01M5V89JDD9XC9ZF80G9QG7KRF` / Run `run_01M5V89PCKAVRJESNTQ77P6KPR` |
| v2 Source2（proof.txtだけ変更） | `sha256:869b6426ce564c9f93d2ac01ded2937f22e822ec1b62ac2b49754f57c4164c14` | `sha256:884ed35a6c7abe1a9daa4078d8371309ed3332a321fa75061cb43987264c3667` | `sha256:9b7afaaadcfceac2988ce00a0a9260834e2c47add4fed32267c77f559e98680f` | Job `fjob_aad3039b67d328ed99f5e8cb4401a2a743029048afbd141974087c3ed8273754` / bundle `bnd_01M5V8BQFT9ATMK8FEN824F9MH` / Run `run_01M5V8BV1PR5N3T52CCZ154PKB` |

上記2 Runはそれぞれexecution.run_id/lease_id/attempt_idに束縛されたfully_satisfied=true Receiptを取得しStop済み。Source1のDは508def…、Source2は9b7afa…へ変化したがKは884ed3…のまま。transport SHA、元Archive SHA、Source treeのdigestを別々に記録している。

Rust source_evidence: Archive4f6e…のtree_digestは8b4ba6c0…、Archive869b…はee6e2905…。v1/v2は同じSourceRevision（srev_01M5V53V8NXCVCBB9P1YKZQT56 / srev_01M5V6868RTT6Y07CVFS9FZSS9）を共有したが、全5 Resultのsource_materialization_idはnull。API Source表示digestは別表現なのでRust tree digestやArchive digestへ読み替えない。

v1 Source: Archive4f6e…から初回schema csch_01M5V53V48DY4NQGP64GB5XPFS、再Formation schema csch_01M5V547J4M10BQ5EBWV8MDB6P。API Source digestはいずれも6c968306…で、file bytes/hashを確認。Archive869b…へのRevisionはschema csch_01M5V6862HTSRNE9062WJVDHSY、API Source digest aab49c92…。旧Result/ReceiptのSHAは変更前後ともa9f46265c712fbbe028712f8823dbcb0df273475ee037fc138b50bbb04072667。

ただしlegacy v1の自動生成Kにはsource-identity観測が入り、proof.txt変更だけでもKが7b47859d…から85a0e9f1…へ変わった。v1の更新成功をFrozen K維持とは扱わず、capsule/2の上記実測と分離する。過去Receiptやidentityを書き換えていない。

`source-reference.status=source_reference_retained`はobject存在確認であり、byte_integrity_verified=falseである。04:33:28Zの期限後試験ではadmission/from-sourceが共に410。永続SourceMaterializationが欠ける経路を再実装やTTL迂回で隠さず、#895へ正本を再利用する受入条件を記載した。

補助証跡: 別セッションのmemos Gate A/B clean-worker記録はOCI image digest固定・同一K/D/bundle・実Run Receipt・Stopを保持する。ただし事前の別pack条件のexpected bundle SHAとは不一致（expected_bundle_sha_agrees=false）であり、その条件までPASSとは扱わない。現sourceとHosted結果の直接対応は本作業の新規Artifact 2件で確認した。

## 回収・残留

本作業が作成した17 Runと17 LeaseをD1で照合し、非終端0件。Source/旧Receipt/Instance/State volumeは継続と監査のため保持した。State fixtureは同一InstanceでStop後に新しいprocessを起動しcounter3を復元した。同一process継続の確認はStopを挟まないReconnect試験で行った。

共有slotが別セッションのRun run_01M5V95TBKN469MCQ0NWNQ853R / Lease01M5V95TBKKMS8QP182X8YNNNYで使用中だったため、初回の最終Terminal再試行はcapacity拒否となった。そのRunを停止せず、解放後の再試行に成功。最終Docker読取は開始時の4 Containerだけで、本作業のContainer残留0件。Production Formation WorkerのPID2809237と起動時刻は開始前後で一致。

## 残課題

| 優先度 | Issue | 次の条件 |
|---|---|---|
| P0 | [API #895](https://github.com/ato-run/ato-api/issues/895) | 一時Uploadと永続Source Archiveを分離し、TTL後も元Archive bytesとDigestを検証して再Formation。承認・所有権・保持Policyを迂回しない |
| P1 | [ato #1582](https://github.com/ato-run/ato/issues/1582) | Receiptの実行環境/Policy/Capability条件を共通Rust authorityで監査可能にする。Process ABIと受付条件も整理 |
| P1 | [ato #1571](https://github.com/ato-run/ato/issues/1571) / [#1572](https://github.com/ato-run/ato/issues/1572) | memosのOCI wake、同一InstanceでのD更新とState/Receipt対応を実機で完了 |
| P1 | [ato #1578](https://github.com/ato-run/ato/issues/1578) | Windows Worker異常終了、CI Scenarioの非再現失敗。Linux成功と混同しない |
| P1 | [PWA #525](https://github.com/ato-run/ato-pwa/issues/525) | baselineと同じ11失敗を、UI/API契約の意図を確認して解消 |
| P2 / 設計確認 | [ato #1577](https://github.com/ato-run/ato/issues/1577) | local-only namespace/WASI/Desktop差分を依存付きで個別評価。devへ無理にCherry-pickしない |

OPENの詳細分類、関連CLOSED223件、95件の包含根拠、追加PRは[PR-INVENTORY.md](PR-INVENTORY.md)。raw logs・各PRの全commit/patch・branch/worktree snapshotはworkspace `.tmp/dev-consolidation-20261026/evidence/`。機密を含まない再検証用IDとReceiptは[validation-evidence.json](validation-evidence.json)へ保存した。

## 最終追加差分の検証

文書PR #1584のmerge時に、別セッションのValidator #1583（334df072…）とAPI #896（16d1c53d…）がdevに統合された。relayの一時503に対する有限retry、ready後のclaim、fence付きのtyped abandonmentが一組の契約となる。APIはtypecheckと関連15 tests PASS、staging active6dab6a21へsafe deployし36 secrets保持・health200。反映後のNative Terminalも14 checks PASS。

Validatorのintegration13 testsは成功したが、全targetチェックで旧ValidationJob fixture3件にretry_budget_msがないcompile errorを発見。既存のfallbackを表すNoneを明示して修正し、fmtを揃えた。修正コードbff38ed81でLinux全Workspace check/tests/fmt/clippyがすべて終了0。変更はfixtureと書式であり、新たなskipはない。Formation Workerの動作コードは4d43ad44のまま。稼働中独立Validatorのbinary SHA256は5d094a8048dd71499bdea196bfbff33206715fe779b8c6f5f77f77bacb40bd84（/usr/local/bin/ato-portable-validator-1571-5d094a8048dd）。Source commitの直接埋込みはなく、稼働binaryとGit commitを混同しない。

共有portable flagは復元後に他セッションによる再有効化を検出したため、portable項目だけを再度OFFへ戻し、終了時のreadbackもOFFを確認。その他の共有設定は保持。Source永続再利用P0は引き続き未達であり、#1571/#1572の完成もこのretry修正だけでは主張しない。
