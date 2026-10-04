# 実UNKNOWN専用の追加受入枠

ユーザーが明示承認した別measurement `formation-native-real-unknown-20261005-01` を準備した。Search最大1、D round最大2、exchange最大3、Runtime attempt最大1、inspection最大2、wall clock最大20分。旧campaignは13/12超過で終了のまま。audit SHA-256 `91262a72bb27ebcc03107b7d0b6081135be78e6868e1d49f7ad5b70ef9470197` の不変を確認した。新campaignのSearch/Native/ledger実行はまだ開始しておらず、消費0。[保存済み計画](formation-native-unknown-supplemental-20261005.json)。

現在のCoordinatorはclaimed attemptのcompletion silenceを30分後にUNKNOWNとする。実workload開始 → completion/ACK transport切断だけを注入すると、20分枠内には真正なUNKNOWNへ到達しない。DB timestampの変更、時計偽装、失効値のテスト専用短縮、旧台帳上限の上書きは行わない。wall clockのみ40分へ変更する案、または20分のままRuntimeを実停止・復旧してdurable started_unfinishedを報告する案を提示しており、追加条件の回答待ち。後者を純粋なACK喪失試験と同一視しない。予算guardと元Searchのdeadline/消費量保持はどちらも維持する。

実行時はowner開始入口のaggregate admissionを通し、同じSource/Kから実NativeのD submit、Runtime claim/実開始、障害後UNKNOWN、Native終了、新Native文脈から同Searchの保存記録照合を行う。新input/D round/attempt/期限延長/予算返却/UNKNOWNの失敗扱いを禁止し、可能なら保存済みresultで同attemptを確定する。fixtureのagent名変更を実Native受入と数えない。現時点でこのgateは未実施で、#1485はDraft維持。

Kuttはこの枠を使わず、既存successful SearchからSource Result/form-verify/通常StateServiceの実Run A/BをPASSした。証跡は#1481/API #731の `docs/ops/formation-kutt-source-result-product-20261005.md` と同名evidence JSONに分離した。Source再探索/Native/LLM API追加0、functional attempt5（2失敗を保持し3PASS）、最後のactive writer/reservation/temporary values0。Kutt成功をUNKNOWN gateの代用にしない。

#1479/#1480はReady for review。model MCPはstatus/next/submitのみ、owner cancel、tokenなしpublic descriptor/private capability分離の契約を維持する。実行pinの3OS CI PASSは既存記録を参照。秘密のFD/RAMだけでの保持を実装済みとは主張しない。old WBO UNKNOWN、独立API残3計画、配備、remote cloud migration、100件測定は今回触っていない。
