# RunPod初回503の調査と起動準備・診断の修正

対象はDraft API #760 / Rust #1493。追加のPod作成、GPU実行、デプロイ、remote migration、マージ、本番反映を行わない調査・ローカル修正。

検証した実装commitはAPI `27c0dee88db29c1a7416c314a5c186dd73cd6c6f`、Rust共通runtime `1fa65053d7d4724d6f6e775b9134577e2867d48b`、Sample `97474b8ee000fc290a86384d1d7146594e57af63`。以後の記録commitは文書のみ。

## 既存証跡の回収

初回Run `run_01M4DX3ZCNDZCR1EYFN3KZ5V9P`について、D1のdiagnostic_reports、同じlease、予約済みAsset/Run Output一覧をread-onlyで確認した。通常のapp route、start、再作成は呼び出していない。

- diagnostic_reports: 0件。leaseのerror_json: null。
- 保存出力: `chat-history.json` 1件、48bytes、saved、Asset available。
- 履歴内容: `{"schema":"ato.gpu-chat-history/1","jobs":[]}`。生成文はない。
- engine stdout/stderrは削除済みPodのscratchにだけ存在したため回収不可。
- 初回のPod消滅は前回のprovider一覧空/実行spend 0の二重確認を証跡とする。今回Podは作成しない。

## 503の切り分け

APIの従来投影はreadinessにApplication Surfaceの`/`、60秒を使っていた。sampleの`/`は準備中でもHTMLを200で返す。Runnerはここでreadyを報告でき、固定Kの`GET /health`をengine準備完了前に観測する。その観測が503だった。

HTTP起動から約31秒という初回記録と、sampleのengine準備期限180秒はこの経路と整合する。**早すぎるContract観測経路は確定したが、初回engineが180秒以内に起動できたかは不明**。診断がなくCUDA照会・driver・モデル読込・全layer offloadの失敗を排除できない。待ち時間だけを延ばして再試験しない。

Ubuntu22.04ではb11429配布物のGLIBC 2.38 / GLIBCXX 3.4.32が不足する事前loaderエラーも確認済み。初回はUbuntu24.04に変更して起動したが、これはCUDA・モデル成功の証明ではない。

## 修正した境界

選択Dの`requirements.startup`（authoring `derivation.startup`）にPort、path、期限、任意ABI下限を束縛する。Rust validatorがAPIへ投影し、APIは既存readinessへ渡す。sampleは`/health`・180秒。旧Dがフィールドを省略したcanonical bytesと動作は維持する。

共通runtimeはprocess_started、http_responding、readyを区別する。503はHTTP応答の証拠でありreadyではない。1つの準備期限の前後でprocess終了・取消・実行認可・Run期限を確認し、期限後の200も拒否する。準備後に固定Kを観測し、失敗HTTP statusを成功するまで再試行しない。Kのstatus/body digestを変更しない。

startupを宣言したDは`runtime_feature=process_startup_v1`を必要とし、対応していない旧Runnerを選ばない。CLIの共通attemptとHosted Process readinessは同じ準備待ちを使用する。Sampleの180秒には展開、ABI/loader検査、CUDA照会、モデル準備全体を含め、各HTTP probeも残り期限以内に制限する。

## ABI・imageの選択

新sampleはGLIBC 2.38 / GLIBCXX 3.4.32を宣言する。RunPodは配置条件のprocessAbiをconfiguredな監査済みimmutable imageと比較し、不足/未知ならprovider lookup/create前に恒久的に拒否する。既存Podを採用する場合も、そのimageが不明/不足なら拒否し、新Podへfallbackしない。

- 元Ubuntu22.04: `runpod/pytorch@sha256:61a4aafb0094cd773f11eefa378929d5a687bd775febeb78eac62fc824141fb5`。新sampleを拒否する。
- 明示選択するUbuntu24.04: `runpod/pytorch@sha256:0a360022e8de4375af99430f84e8b38951acc397252163a37ceac7204d01be35`。確認済みGLIBC 2.39 / GLIBCXX 3.4.32以上を保守的なprofileとする。
- Workerは実GNU ABIをprocess前に再確認する。Sampleは使用するlibstdc++とengine `--help` loaderをCUDA照会前に確認する。

今回はstagingのimageやRunner artifactを変更しない。旧bundleは新要件を持たないため再利用しない。次の判断後に新bundleをpack・Rust validator確認し、対応Runnerとimageをstagingへ反映する必要がある。

## 診断と回収

Sampleは原子的な`startup-diagnostics.json`へフェーズ経過、loader/CUDA照会、HTTP状態、offload数、engine終了コード、log capture結果を保存する。stdout/stderrは別ファイル各256KiB末尾、probe textは各2,048文字に制限する。管理keyやenv全体は出力しない。

engine異常終了ではアプリ自身も非zeroで終了する。準備中SIGTERM、timeoutはengineをterminate/kill/reapし、診断と空の履歴を確定する。共通Runnerのprocess-startup JSON、512KiBまでのprocess log、失敗時stop証拠も、停止確認後に既存Run Output/Asset経路へ渡す。起動失敗時もterminal failureがData Grantを失効させる前に保存する。最大7ファイルで既存grant上限8内。保存応答・停止確認が失敗した場合は成功としない。最終Pod削除期限は延ばさない。

## ローカル検証

- Sample Python: 13件成功。遅延503→200、engine exit 7、準備timeout、準備中取消、実SIGTERMでmain/engine終了、診断・空履歴保存、ABI不足の起動前拒否、bounded logをCPU実process/実HTTP/Unix socketで検証。
- 共通Rust: `cargo fmt --all -- --check`、`cargo clippy --all-targets --all-features --offline -- -D warnings`成功。`cargo test --workspace --offline`は1,958件成功、9件既存ignore。readiness deadline後の200拒否、取消、process回収、固定K観測1回、診断の既存Asset reserve/part/complete HTTP保存、ABI/schema/golden互換を含む。
- API: typecheck成功。関連10suitesの最新結果で計196件成功。startup DTO/projection、RunPod create/adoption前ABI拒否、warm reuse、出力保存・grant・provision・lease dispatch・v1/v2 wireを確認。
- APIのschema:check、schema:test-bootstrap、schema splitter 3件成功。既存#759/#760ローカル統合tree `73feb62c`でもschema:check成功。新migration/番号変更/remote適用なし。

最初の全体検査はこの作業のbuild cacheでディスクが満杯になり、Rust書込とWorker SQLiteが失敗した。自分のdebug buildだけをcargo cleanし、CARGO_INCREMENTAL=0で回復した。API一括実行ではdispatch DB初期化が10秒timeoutとなったが、同じ10秒制限の単独実行で9件成功。Rust初回workspaceではCLI computation_architectureのJSON EOFが1件出たが、同条件の再実行で全体成功。変更前`bacbffff`の独立worktreeでも該当suite 13件成功し、EOFの原因は未確定の一過性失敗として記録する。新skipやtest timeout延長は加えていない。

fixtureのCUDA表示とoffload文は模擬。実生成・実CUDAのAcceptanceではない。追加CI課金を避けるため通常のpush/pull_request workflowは`[skip ci]`で省略した（[GitHub公式手順](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/skip-workflow-runs)）。APIの新headはcheck/runなし。RustではGitHubの自動CodeQLが`dynamic` eventで起動し、この指定の対象外だったため停止した。ローカル成功を全CI成功とは報告せず、repository/workflow設定は変更していない。

## 残る実機範囲・費用

生成文の実Asset保存、時間制限による自動停止、保存猶予切れは実GPU未検証のまま。初回の費用は既存報告のRunPod残高差分$0.0177438185（請求額ではない）、R2約1.88GB等の保管費用は精算未確定。既存保管物を維持し、今回追加の有料computeを作成しない。

2回目は別途判断。判断後もGPU単価$1.10/時以下、同時1台、小型LLM、再作成なし、全試行累計+storage等を含む$3の超過し得る目標、異常時削除優先を維持する。
