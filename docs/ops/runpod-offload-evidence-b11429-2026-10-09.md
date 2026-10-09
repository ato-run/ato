# b11429 offload証拠の調査・修正（追加有料Podなし）

対象はRunPod小型LLMの2回目失敗。3回目は作成していない。
API、Runner、image、共有staging設定、migration、feature flagは今回変更していない。
Rust repoのsampleと診断・回帰試験だけを修正し、PRはDraftとする。マージ・デプロイは行わない。

## 原因と前回ログ

固定engineは公式b11429、source revision
`d81235049384534c167caea52b85a694f6103d14`。
配布archive SHA-256は
`b13c64b9224d3c89d9993945d70cc21177ad00178c21b8d34237e3bc139834f1`。

前回Assetのstderrは1164 bytes、stdoutは0 bytes。
診断はCUDA0認識、engine HTTP200、`offload_layers=null`、
`log_capture_complete=true`、`log_errors=[]`、保存上限は各262144 bytes。
実stderrは`verbosity = 3`とmodel loadedを含むが、offload集計・層配置は含まない。
engineは後の停止処理でexit0であり、異常終了を示す記録ではない。
回収Assetと元manifestのSHA-256をfixtureへ保持した。

固定sourceの
[offload集計](https://github.com/ggml-org/llama.cpp/blob/d81235049384534c167caea52b85a694f6103d14/src/llama-model.cpp#L1926)
は`offloaded N/N layers to GPU`を今もlibrary INFOで出す。
文言変更ではない。
[library logの変換](https://github.com/ggml-org/llama.cpp/blob/d81235049384534c167caea52b85a694f6103d14/common/log.cpp#L533)
ではINFOをverbosity4、DEBUGを5へ変換し、threshold3では両方を抑制する。
[common callbackの登録](https://github.com/ggml-org/llama.cpp/blob/d81235049384534c167caea52b85a694f6103d14/common/common.cpp#L397)
がlibraryのlogをこのfilterへ接続する。
plain INFO/DEBUGはstderr、`--log-jsonl`はstdoutである。
[loggerの出力先](https://github.com/ggml-org/llama.cpp/blob/d81235049384534c167caea52b85a694f6103d14/common/log.cpp#L93)
と実配布libraryのcallback replayの両方で確認した。
配布libraryには該当loggerが存在し、compile時の全面抑制はない。

したがって、今回の欠落は**既定verbosityでの出力抑制**と判断する。
収集量は上限未満でcapture完了、sourceと配布loggerで同じ抑制を再現できたため、
収集時のtail欠落を原因とする証拠はない。
ただし、前回Podの実GPU配置は回収ログから復元できない。
「offloadされなかった」も「全offloadされた」も確定しない。

## 取得する証拠と判定

固定版の`/props`はmodel層のdevice配置を提供しない
（[get_res_props](https://github.com/ggml-org/llama.cpp/blob/d81235049384534c167caea52b85a694f6103d14/tools/server/server-context.cpp#L4955)）。
構造化されたJSONL envelopeの`type/level/msg`から、固定sourceのmetadataと
[層ごとの割り当て](https://github.com/ggml-org/llama.cpp/blob/d81235049384534c167caea52b85a694f6103d14/src/llama-model.cpp#L1591)
を抽出する。メモリ量を層数へ換算しない。

固定Qwen GGUFは`general.architecture=qwen2`、`qwen2.block_count=28`。
model SHA-256は
`6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e`。
対象はrepeating28層＋output1層の29個。input embeddingは固定engineがCPUに置く別対象。

準備成功には全て必要：

1. engine HTTP200とCUDA照会成功。
2. model metadataの`n_layer_all=28`。
3. index0〜28それぞれのdevice割り当てがCUDA0。欠落・別device・重複矛盾なし。
4. 集計29/29と一致。
5. tensor buffer fallback/overrideの診断なし。

集計はsource上で指定layer数に依存するので、それだけで配置を証明しない。
層割り当て後にCPU bufferへfallbackする可能性もあるため、
[done_getting_tensorsのfallback診断](https://github.com/ggml-org/llama.cpp/blob/d81235049384534c167caea52b85a694f6103d14/src/llama-model-loader.cpp#L1403)
が出れば成功を拒否する。この診断は最初のtensorと残り件数だけなので、
残りが全てoffload対象外であるとは仮定しない。
CPU inputのhost/repacking buffer変更を避けるため、`--no-host --no-repack`も明示する。
これは層のdevice割り当てとmodel load完了の証拠であり、全演算kernelのCUDA実行測定ではない。
実GPUでこの組み合わせを確認する試験はまだ実施していない。

sampleは`--log-verbosity 5 --log-jsonl`を明示する。
`offload_evidence.py`はstdout/stderr別のincremental parserで各行16KiB・配置最大29件。
raw logの256KiB tail切り捨て前に抽出し、診断へ配置・対象数・集計・矛盾を保存する。
配置数が未証明/矛盾なら`gpu_placed_layers=null`とし、観測済み件数を別fieldへ保存する。
証拠なしをGPU配置0層という結論へ変換しない。
stdoutへ移ったログも既存の共通Asset保存枠へ渡す。管理key/envは追加保存しない。
固定Kと180秒の準備期限、Run/Pod期限、保存枠は変更していない。

| 観測 | error / failure_reason | ready |
| --- | --- | --- |
| HTTP200、配置証拠なし、期限切れ | startup_timeout / offload_evidence_missing | 拒否 |
| engine未起動、期限切れ | startup_timeout / engine_not_started | 拒否 |
| engine開始済み、HTTP準備未完了、期限切れ | startup_timeout / model_preparing | 拒否 |
| 全配置が揃い、CPU/他deviceを含む | offload_insufficient | 拒否 |
| 矛盾、fallback、override | offload_evidence_invalid（詳細はinvalid_reason） | 拒否 |
| engine異常終了 | engine_exited:code | 拒否 |

全経路で停止・reapと診断確定へ進む。503を成功にせず、Contractを弱めたり再観測し続けたりしない。

## ローカル検証と出所

fixtureの[README](../../tests/fixtures/portable-gpu-llm/b11429/README.md)と
`provenance.json`にSHA-256と再現手順を記録した。

- **前回の実Asset**：HTTP200だが配置未証明をそのまま再現。一般timeoutにmissing理由を併記し、停止・診断保存を検証する。
- **実配布logger**：固定sourceのC++形式を配布libraryへ渡し、verbosity3抑制、4の集計だけ、5の層配置、JSONLstdoutを確認した。full/partial/fallbackの配置値は模擬callback入力であり、GPU実行ではない。
- **実modelの否定例**：既存CPU host、固定Ubuntu24.04、配布CUDA package、固定Qwen GGUFでCPU-only load。HTTP200、0/29 GPU配置、engine exit0、一時コンテナ消滅を確認した。ネットワーク/GPUなしの起動、60秒上限。生成は実施していない。
- **回帰試験**：全/部分/証拠なし、metadata/集計/重複矛盾、buffer fallback、chunk境界、log tail切り捨て、遅延503→200、異常終了、準備取消、ABI拒否、診断保存・reapを確認する。

Pythonの全29試験は成功し、CPU fixtureのverbatim excerpt整理後も関連11試験が成功。
Rust format・Clippy（all-targets/all-features）は成功。
並列workspace試験では変更外のlocal stop ACK読み取りが空になる一時失敗があり、
該当CLI integration全16試験を単独再実行して成功した。
別の並列実行ではHTTP preparation fixtureのstream readがWouldBlockとなりtimeoutした。
これらの失敗ログはlocal記録へ保持した。
最終workspace直列試験は1958 passed / 0 failed / 9既存ignored（161 targets）。
未証明配置数のnull整理後もPython全29試験、Rust sample bundle試験1件、新pack/validator確認が成功。
CIは追加課金を避けるためcommitの`[skip ci]`で起動しない。CI成功済みとは扱わない。
core/runtimeのRust実装・試験を変更して成功させる修正は行っていない。
実GPU結果、ローカル結果、未実行CIを混同しない。
実ログfixtureはsampleの外に置き、実行bundleへ診断証跡を混入させない。
既存の128KiB以下というsample bundle試験条件は維持する。

新sourceをlocal CLIから新規packした結果（source基準main `ff5265485544b512f5aba86488fac5c717cd20f1`）：

| 対象 | 固定値 |
| --- | --- |
| bundle bytes | 105651 |
| bundle SHA-256 | `b9f859a9c83bcdf8e60fa3db53763ae441fbe78daea3e9cab4ee483614089a0e` |
| ContractRef | `sha256:cd8d845da4e6824f015e722b3a1921dec69be80df83b8d20e202de57addc6494`（維持） |
| DerivationRef | `sha256:f7e43c16da2f038005b3d27d47be26aab6420b443c2abddd5daaa2fb215d2009` |

freshなCLI環境を2つ使い、pack結果のbyte一致を確認した。
実Rust validator binaryをlocalhostのvalidation-job fixtureへ接続し、bundle transport digest、
固定K/D、startup180秒、GLIBC2.38/GLIBCXX3.4.32、2つのModel Setと3外部object要件、
schema/digest/closureを検証した。
workspace8ファイルには新しい`offload_evidence.py`が存在し、実ログfixtureは含まれない。
これはbundle構造検証であり、GPU実行・Run acceptanceのreceiptではない。
出力bundle/reportは作業場所の`.tmp/offload-evidence-20261009/`へ保存し、stagingへuploadしていない。

## 次回判断に残るもの

3回目の有料Pod作成、実GPU全29層配置、固定Contract成功、非空生成・SSE・取消・冪等再送、
通常Run期限による停止とcanonical ACK、生成履歴のAsset保存・Pod削除後のSHA照合は未検証。
保存猶予切れ、次Runへのrestoreも未検証。
今回の修正はまだstagingへ反映していない。旧bundleの再利用はしない。
次回承認前に新sourceのbundleと実験対象digestを固定し、共有設定と期限・累計費用を計画へ反映する必要がある。
