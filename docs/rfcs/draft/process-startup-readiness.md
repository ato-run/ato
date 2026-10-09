# Process起動準備と固定Contract検証の境界

Status: draft。RunPodの起動準備と診断境界。2回目はHTTP200まで確認し、全offloadの証拠不足で失敗。3回目は未承認。

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

## 固定engineのoffload証拠

このsampleのb11429はrevision `d81235049384534c167caea52b85a694f6103d14`。
libraryのINFOはcommon loggerのverbosity 4へ変換され、既定3ではoffload集計が抑制される。
`--log-verbosity 5 --log-jsonl`で実際のloggerのJSONL envelopeをstdoutへ取得する。
`/props`は層配置を返さない。sampleは固定版の`print_info: n_layer_all`、
`load_tensors: layer ... assigned to device ...`、`offloaded N/N layers to GPU`を抽出する。
固定Qwen GGUFの28 repeating layersとoutput layerの計29個について、各indexの配置先が
CUDA0であること、model metadataが28であること、集計が29/29であることを要求する。
input embeddingはengineがCPUに置く別対象であり、この29個に含めない。
これは層のdevice割り当てとload完了の証拠であり、すべての演算kernelの実行場所を測定するものではない。

層割り当て後のtensor buffer fallbackを見落とさないため、その固定版DEBUG診断があれば
全配置の成功を拒否する。CPU inputのhost/repacking buffer変更をGPU fallbackと混同しないよう、
`--no-host --no-repack`を明示する。配置overrideも拒否する。
CUDA照会、起動argv、HTTP200、GPUメモリ量、集計だけでは成功しない。
配置欠落・別device・重複矛盾・model/集計不一致も成功しない。

JSONLはstdout/stderrを別々にincrementalにparseし、各行16KiB、配置最大29件に制限する。
各256KiBのraw log末尾を切る前に証拠を抽出し、`startup-diagnostics.json`へ配置と集計を残す。
未証明/矛盾時の`gpu_placed_layers`はnullとし、観測されたCUDA配置件数は別fieldへ記録する。
証拠なしをGPU配置0層と解釈しない。
`error=startup_timeout`に加え、HTTP200まで到達して配置が未証明なら
`failure_reason=offload_evidence_missing`、未起動なら`engine_not_started`、
HTTP準備未完了なら`model_preparing`を記録する。確認された部分配置は
`offload_insufficient`、矛盾/fallbackは`offload_evidence_invalid`で期限前に失敗する。
停止・reap・診断保存の共通経路は維持する。

## 検証と残る範囲

追加課金なしで遅延503→期限内200、engine異常終了、準備timeout、準備中取消を実process/HTTP fixtureで確認する。Contract検証が準備中に行われず、不成立を成功まで繰り返さないことを確認する。
初回の保存済み診断indexは0件、lease errorはnull、出力Assetは空の履歴1件だけだった。engine scratchログはPod削除で消滅し、CUDA/モデル失敗は排除できない。
実生成文保存、時間制限自動停止、保存猶予切れは引き続き実GPU未検証。
2回目の実stderr、配布loggerへの固定ソース由来callback、実QwenのCPU配置ログを回帰入力とする。
callbackのGPU配置値は模擬入力であり、実GPU acceptanceとは扱わない。
