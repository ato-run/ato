# 受入と終了報告

## 証拠の区分

固定応答fixtureはプロトコル検証であり、実Codex/Claude Code探索の証拠ではない。実エージェントの受入は承認済みの新しい小規模計画を登録してから行う。PlanにはSource digest、固定K/ContractRef、利用可能Runtime、exchange/round/inspection/attempt/byte/時間上限、sandbox ceiling、deadline、測定IDを記録する。

両エージェントとも新しい文脈でSource/K/catalogから開始し、成功D・過去測定・アプリ別回答を入れない。計画を提示しただけでは実行の承認と扱わない。元の測定pin・失敗Search・UNKNOWN・台帳を後続結果へ書き換えない。

| 受入 | 保存する証拠 |
|---|---|
| 実エージェント探索 | 実製品の配置/明示起動、agent種別/version/model、known-Dなし入力、inspection、proposal、実Runtime attempt、fresh same-K receipt。 |
| D修復 | 実行失敗の観測、次のD差分、同じSource digest/ContractRef、後続attemptのreceipt。型修正だけと区別する。 |
| 再接続・競合 | 応答保存後切断、ACK消失、同一再送、異内容/古い応答、二重接続でのattempt数・消費枠・deadline不変。 |
| 停止 | budget/deadline/needs_input/権限拒否/UNKNOWNの状態、停止・ACK・予約解放・cleanup。 |
| 非露出 | Private側で生成した検証値に対するProducer入力、エージェント出力、エラー、公開ログの確認。生成値は公開証跡へ記載しない。 |
| 互換性 | 旧Codex Session config/digest、独立API経路、既存receipt/台帳の読取が維持されること。 |

Runtime fixtureは制御されたケースとして記録し、unique OSSへ加算しない。製品の起動/HTTP 200だけではKのPASSとしない。機能・永続化・再起動は別に許可されたRunで同じ探索artifactを用いて確認する。探索PASSから通常Run権限を追加しない。

## 最終報告

以下を保存済み証拠で埋め、不明は`unknown`、未実施は未検証とする。

- 測定ID、Search ID、固定Source digest、ContractRef、選択されたDerivationRef。
- Ato/API/Runtimeの実行pinと環境、agent種別・製品version・取得できたmodel。
- attempt IDとfresh same-K receiptの所在/digest/`fully_satisfied`。後続pinへ付け替えない。
- exchange、D round、inspection、Runtime attempt、経過時間、それぞれの使用/残枠と元のdeadline。
- エージェント内部の実LLM call/token/費用。取得できなければ`unknown`。exchangeを実callと数えない。
- candidate停止、結果ACK、未精算予約、owner入力cleanupの状態。各証拠がない項目を完了としない。
- 現在のSearch状態と停止理由、未検証の機能・保存・resume・非露出・CI。

Session成功は独立API成功として計上しない。マージ、配備、remote migration、flag有効化、通常Run許可、100件再測定は別判断である。
