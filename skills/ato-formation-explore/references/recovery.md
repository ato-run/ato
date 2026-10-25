# 再接続・競合・終了の復旧

ownerは保存済みUNKNOWNに対して、Native launcherの`--reconcile-only`を選べる。新しいNative文脈へ公開するのは`status / next`だけで、同じSearchの保存状態を報告して停止する。期限後も報告だけを最大120秒で行い、owner campaignのwall clockが先ならそこで止める。元Searchのdeadlineを変更せず、新しい探索入力・D・inspection・実行は取得できない。報告turnのSDK usageは追加の探索exchangeと混ぜず、内部call・実請求が不明なら不明とする。期限後のACK・result照合・cleanupはownerの既存経路で継続する。

launcherはowner MCP brokerにも`--reconcile-only`と報告windowの`--relay-expiry-ms`を渡す。brokerは保存済みUNKNOWN・未解決attempt・`input: null`を確認し、署名対象のrelay bindingへ読み取り専用scopeを固定する。`submit`はtool一覧から除外し、直接呼び出されてもdispatch前に拒否する。通常relayの期限は元Search deadlineを超えられない。broker更新前のbinaryは期限後接続を拒否するため、その失敗を再探索で置き換えない。

再接続時は`status`で保存済み応答、未ACK結果、実行中attempt、終端理由を確認し、その後`next`を取得する。結果の不明を新たな推論やRuntime attemptの理由にしない。どの段階でも元のdeadlineと消費枠を維持する。

| 保存済み状態 | Producerの操作 |
|---|---|
| 入力だけが保存済み | nextがsubmitを指示し、Searchとexchangeの期限内なら、同じexchange ID/digestの入力へ応答する。 |
| 応答保存後に提出結果が消失 | statusで保存を照合する。同じ応答だけを再送できる。新しいDを作らない。 |
| 応答提出後に次の入力がまだない | Bridgeの待機理由に従う。所有者によるvalidation/attempt/ACKが完了するまで重複実行しない。 |
| ACK消失・attempt実行中 | Owner側のattempt/結果/ACK journalを照合してもらう。ProducerにRuntime実行権を移さない。 |
| 古い応答・別内容の競合 | 拒否を維持し、保存済み応答を採用する。接続を増やして上書きしない。 |
| 二重接続 | 同じ保存済み入力を取得してもexchangeを追加しない。最初の受理済み応答が確定し、異なる応答は拒否される。 |
| UNKNOWN・停止未確認 | 継続を止め、未解決effectと最後の証拠を所有者へ渡す。成功・cleanup済みと報告しない。 |
| `needs_input` | 値を要求せず、公開された名前・kind・resource・operation・phaseなどのslot制約を所有者へ渡す。入力・許可後は同じSearchへ接続する。 |
| 予算/deadline/権限/利用上限/応答停止 | 適切な停止理由を残す。枠の補充や推論APIfallbackは行わない。 |
| 終端Search | 再開・resetしない。新計画は別測定IDで事前承認を受ける。 |

Agentは進めない理由をtyped declineで表し、取消を自ら実行しない。明示的な取消依頼はOwnerへ引き継ぎ、Ownerが[protocol.md](protocol.md)のCLI `cancel`を使う。取消要求はCoordinatorへの送信前に保存され、応答が消失しても新しい推論応答を禁止する。取消要求後の状態が不明ならOwnerへ照合を引き継ぎ、再接続で取消要求を忘れない。終了・取消はRequester/Coordinatorの既存経路で行い、candidate停止、結果ACK、未精算予約、input cleanupを保存済み証拠で照合する。Producer接続の切断だけをSearch取消と扱わない。`cleanup: not_confirmed`や所有者側の停止未完了は、その状態で引き継ぐ。

引き継ぎにはSearch ID、固定Source/K、最後のexchange ID/input SHA-256、現在の待機・停止理由、未ACK/実行中/UNKNOWNの有無、元の期限と残枠を含める。接続capabilityやprivate値を貼り付けない。
