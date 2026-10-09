# Process起動準備と固定Contract検証の境界

Status: draft。初回RunPodの503を受けたローカル修正。GPU再試験は未承認。

## 起動と検証

Process開始、HTTP応答可能、準備完了は異なる観測である。既存RuntimeLaunchSpecのHTTP readinessを使用し、選択Dが明示する`requirements.startup`をauthoringの`derivation.startup`から束縛する。
`port`、`path`、`timeout_ms`はrealizationの準備条件であり、Kの観測・期待status・body digestを書き換えない。省略時は既存経路の動作とcanonical bytesを維持する。

Runtimeは1つの開始時刻・期限を使い、HTTP readinessの200系応答を待つ。503は応答可能だが準備未完了として記録する。準備待ちは停止要求・実行認可・Run期限で中断し、終了したprocessは期限を待たず失敗する。期限後のreadyは採用しない。
準備完了後に元の固定Kを1回観測・検証する。K不成立を準備待ちとして再試行せず、失敗のまま停止・保存へ進める。

## ABIとイメージ

Process routeのstartupは任意の`abi`（`glibc_min`、`glibcxx_min`）を明示できる。値は数値成分のversionで比較し、unknownを満足としない。
RunPodはconfiguredなimmutable imageの監査済みABI profileを照合し、未知/不足ならProvider通信・create前に拒否する。別imageへ自動fallbackしない。Workerもprocess開始前に実行環境のABIを確認する。
今回のllama.cpp配布物はGLIBC2.38 / GLIBCXX3.4.32を要求するためUbuntu22.04 profileを拒否し、明示選択したUbuntu24.04 profileだけを許容する。次のbundleを再pack・validator確認する必要があり、旧試験bundleを再起動して互換性を確認してはならない。

## 診断と停止

Sampleは準備フェーズ、CUDA device照会、engine stdout/stderr、終了コード、モデル読込とGPU offloadの証拠をboundedな出力へ原子的に保存する。engine異常終了では診断を確定してprocess自体も失敗し、readyを報告しない。取消・timeoutでもengineを停止・reapし、診断を確定する。
Workerは起動失敗経路も共通の出力保存へ接続する。停止確認後にだけ保存し、保存応答後に失敗/停止を報告する。未確認の停止・保存失敗を成功と扱わず、Podの最終削除期限は延ばさない。秘密envや管理キーを診断へ含めない。

## 検証と残る範囲

追加課金なしで遅延503→期限内200、engine異常終了、準備timeout、準備中取消を実process/HTTP fixtureで確認する。Contract検証が準備中に行われず、不成立を成功まで繰り返さないことを確認する。
初回の保存済み診断indexは0件、lease errorはnull、出力Assetは空の履歴1件だけだった。engine scratchログはPod削除で消滅し、CUDA/モデル失敗は排除できない。
実生成文保存、時間制限自動停止、保存猶予切れは引き続き実GPU未検証。
