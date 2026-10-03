# 再接続・競合・終了の復旧

再接続時は`status`で保存済み応答、未ACK結果、実行中attempt、終端理由を確認し、その後`next`を取得する。結果の不明を新たな推論やRuntime attemptの理由にしない。どの段階でも元のdeadlineと消費枠を維持する。

| 保存済み状態 | Producerの操作 |
|---|---|
| 入力だけが保存済み | 同じexchange ID/digestの入力へ応答する。 |
| 応答保存後に提出結果が消失 | statusで保存を照合する。同じ応答だけを再送できる。新しいDを作らない。 |
| 応答提出後に次の入力がまだない | Bridgeの待機理由に従う。所有者によるvalidation/attempt/ACKが完了するまで重複実行しない。 |
| ACK消失・attempt実行中 | Owner側のattempt/結果/ACK journalを照合してもらう。ProducerにRuntime実行権を移さない。 |
| 古い応答・別内容の競合 | 拒否を維持し、保存済み応答を採用する。接続を増やして上書きしない。 |
| 二重接続 | 同じ保存済み入力を取得してもexchangeを追加しない。最初の受理済み応答が確定し、異なる応答は拒否される。 |
| UNKNOWN・停止未確認 | 継続を止め、未解決effectと最後の証拠を所有者へ渡す。成功・cleanup済みと報告しない。 |
| `needs_input` | 値を要求せず、必要なpurpose/scope/phaseメタデータを所有者へ渡す。入力・許可後は同じSearchへ接続する。 |
| 予算/deadline/権限/利用上限/応答停止 | 適切な停止理由を残す。枠の補充や推論APIfallbackは行わない。 |
| 終端Search | 再開・resetしない。新計画は別測定IDで事前承認を受ける。 |

明示的な取消依頼には[protocol.md](protocol.md)の`cancel`を使う。終了・取消はRequester/Coordinatorの既存経路で行い、candidate停止、結果ACK、未精算予約、input cleanupを保存済み証拠で照合する。Producer接続の切断だけをSearch取消と扱わない。`cleanup: not_confirmed`や所有者側の停止未完了は、その状態で引き継ぐ。

引き継ぎにはSearch ID、固定Source/K、最後のexchange ID/input SHA-256、現在の待機・停止理由、未ACK/実行中/UNKNOWNの有無、元の期限と残枠を含める。接続capabilityやprivate値を貼り付けない。
