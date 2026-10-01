# Formation: Codex-first shared reasoning prototype

今回のappendは**未完了**。CodexとAPIが共通で使う入力・inspection・型付き出力・durable journalを実装した。Codex自身の推論による実OSSのfresh same-K PASSはまだない。旧100件のbaseline 7/100、exploration 7/100、追加0と、以前のAPI SVGOMG修正成功appendは変更していない。今回の値を旧100件へ加算しない。

## 固定条件とコード

- 対象は事前固定のSVGOMG（index 57）とchangedetection.io（index 7）。repo/ref/archive/license/Kは `formation-codex-prototype-selection.json` に記録。
- 初期CLI/workerは `fb140026bb555d01f5446cd1a92f04affac4f9b6`。context重複修正後は `688eb4a6484bf8e0237c6265a64aa0d590b16488`。
- Rust receipt authority WASMは `fb140026bb555d01f5446cd1a92f04affac4f9b6`、SHA256 `6313907503f9b18fc01fa212ad7b52b79bafc5d6b2bbec5caf5bcabe933ef28a`。Core部分は修正pinとの間で変更していない。API JSは `4ff88c038e095951e2277b759ac7ae012148e02e`。
- Linux/aarch64、Rust 1.96.0。開始時空き80,030,150,656 bytes、証跡収集時空きはhash付きresource.jsonに保存。disk floor 20 GB。
- known-Dなし、同じsource-bound StaticFiles K。changedetection.ioは旧baselineでK未形成のため明示K入口であり、automatic baseline改善とは扱わない。
- 3round、4inspection、取得32 KiB、送信4file/16 KiB。依存取得・buildの凍結host上限とruntime egress deniedを維持。通常Run grant・deploy・remote migrationなし。

入力は `ato.formation-reasoning-input/1`。frozen K/source identity、public inventory、取得済みcontext、operation/toolchain、凍結ceiling、過去D/失敗証拠、残予算を含む。projectionによる省略・切詰めを保存する。private authorization map、credential、physical source path、host identityはmodel inputへ含めない。正確な入力hashに結び付いたsession出力を保存してから既存receiverへ送る。session token・費用は取得できず、APIのusageとして捏造しない。

同一round内のinspectionはworker側で上限・source ref/hashを検証し、凍結sourceのみ取得する。最終proposalはAtoのvalidator/compilerがcanonical Dへ変換する。claim/fenceとAPI送信journalは再開しても同じ状態を使う。未確認UNKNOWNのretryや、round/費用/deadlineのリセットは行わない。

## 実際の観測

| 対象 | 保存入力 / 応答 / 採用済み交換 | round | generated D / 実行 / receipt | 結果 |
| --- | --- | --- | --- | --- |
| SVGOMG | 3 / 2 / 0 | 3 | 0 / 0 / 0 | context修正調査中に旧Searchがexhausted |
| changedetection.io | 4 / 4 / 4 | 3 | 0 / 0 / 0 | inspection後、unsupported_dependency_operationで辞退、最後はno_progress |

SVGOMG Search `search_4d4a1f9208a3285e5ce90497386dd124` ではpublic catalogとinventoryの重複が入力予算を消費し、package.jsonのscriptsが省略された。重複除去と、取得済みだが省略されたfileの再inspection許可を修正した。元Searchを停止・再開した際も期限とroundは維持したため、初期のinspection応答2件は遅延/未採用、最終入力にも応答が間に合わなかった。これはappのbuild failureではなく、prototype運用・入力projectionのinfra failure。source programは起動していない。

changedetection.io Search `search_b7b07e0e6de8ca11712be25ac94381bb` ではround1内にDockerfileとchangedetection.pyを追加取得した。同じround seqの次入力へsourceを取り込み、合計2file/6,766 bytesを記録した。requirements.txt/setup.pyは範囲指定・hashなし依存を使用している。現行operationはhash固定binary wheelを要求し、source-owned requirements解決は表現できない。Dockerfile経路もbuilder recipe未bind、native setupと追加hostが凍結条件へ適合しない。勝手にpip/source書換え/権限追加で補わずtypedな辞退を返した。実行前停止なのでRuntime failureとは数えない。

辞退後のround2/3には新しいsource情報やDがなく、同じ辞退が続いた。inspection付きround後の停止判定には改善余地があり、「3段階の探索が進んだ」とは扱わない。初期Python entrypoint heuristicはlibrary App.pyを選んだ点、manifest/新entrypointのprojectionがなお切り詰められる点も残課題。

## オフラインproposal review

保存済みSVGOMG `r003_s001.input.json`だけからNode/npmの型付きproposalを作り、既存Rust validator/compilerへ通した。package.jsonのbuild scriptとgulpfileのbuild出力先を根拠とし、npm ci → source-owned build → static serve、runtime app.http bind、network要求なしを実行前に記述した。manual startupやsource変更は行っていない。

canonical D: `sha256:33e70161a9b0819520312531e78b1e379538ad21f81906b34707b732d7ee151d`。
K: `sha256:6b2244326026e073d2c7d485434bb4d31d72fc43f06a3bc829c3dbfc6b1666d1`。

これは**元Searchへ未提出・Runtime未admit・未実行**。正しい起動やsame-K PASSを証明しない。proposalとcanonical recipeは `formation-codex-svgomg-review-proposal.json` に保存。期限を消費し終えたSearchの後続として予算をリセットしない。別の独立受け入れ検証については、ユーザーの回答待ち。

## APIと費用

API adapterは同じ入力schema・inspection protocol・出力schemaを使用し、Codex成功Dをseedにしない構造を実装した。共通入力を正確に送信するtransport mockとusage集計は検証したが、今回のlive API armは**未実行**。Codexの実OSS fresh PASSという事前gateが未充足のため、credentialを再読取していない。

- live CandidateProducer / DecisionProvider calls: 0 / 0
- API token / actual cost / 予約消費: 0 / 0 / 0
- 残余: 564,906 USD micros。計画最大12 callsの予約117,972 microsは範囲内。
- Codex session token/cost: unavailable。追加PASSあたり費用は分母0なので未定義。
- unresolved API reservation: 0。旧WBO UNKNOWNは別のhistorical stateとして保持し、再実行していない。
- bridge active elapsedは各交換に記録。SVGOMGの停止中・再開待ち時間を含む総推論latencyとしては使用しない。

## 検証と状態

Core探索22 tests、worker proposal40 tests、関連Clippy/fmt PASS。API関連44 unique tests/typecheck PASS。Linux/aarch64のsource-deleted retained replayはPython/static/Nodeの3件ともPASS。これらはfixture/transport/回帰検証であり、Codexの実OSS fresh receiptではない。

ATO evidence head `ff9ecc98e2f9a8724c733c9314b52fd63f4ea13b` の [CI 36809465786](https://github.com/ato-run/ato/actions/runs/36809465786) はUbuntu PASS、Windows Unix API compile error13件、macOS `a_process_run_is_owned_by_its_run_until_stopped` のInstance worker activation failure。既知の観測と同じ失敗名だがexact base実行で再現したとは記載しない。

API head `4ff88c038e095951e2277b759ac7ae012148e02e` の [CI 36807909306](https://github.com/ato-run/ato-api/actions/runs/36807909306) は失敗。activity-control-planeはCORS audit（x-request-id/x-ato-attempt-fence）とActivitiesの503/D1 undefined、instance-state-syncはcoop-presence WebSocket handshakeで停止した。今回はjobが実行されており課金由来ではない。exact base再現・原因切分けは未実施で、CI greenとは扱わない。

implemented: shared reasoning boundary / durable inspection / session adapter / API adapter。
measured: 2 Codex cases、片方infra correctionあり。
verified: same-round inspection・typed decline・offline canonicalization・retained replay。
未verified: Codex実OSS初回PASS/修正PASS、権限縮小fresh receipt、独立API探索fresh receipt。
merged/deployed/remote migration: false。PR #1452/#712はDraft維持。

## 証跡

`formation-shared-reasoning.json` は `reasoning-evidence.py` により全raw hashを検証して再生成した。公開可能な33ファイルはLinux owned directoryとlocal .tmpに保存。bundle SHA256: `c6be15085e2d0d6de0d6ca3b21b4d2b3a04567557bdf477b63bce75b67739349`。manifest SHA256: `d2d1079e109b3e621a77d881f84e05e4aae2dff9bcd30b38908587f2ac3ff4ac`。

`formation-shared-reasoning-raw-manifest.json` は公開fileのhashと、exportしないowner-only stateのhash/保持場所を区別する。claim/fence、coordinator token/database、private authorization、expanded sourceは推論contextへ出さず、元hostで保全した。source archives、元Search、過去ledger・receipt・stateを削除していない。
