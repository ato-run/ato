# 実UNKNOWN専用の追加受入

ユーザーが40分案を承認したため、`formation-native-real-unknown-20261005-01`を実Codex 0.160.0 / gpt-6.1-solで実行した。上限はSearch 1 / D round 2 / exchange 3 / Runtime attempt 1 / inspection 2 / wall clock 2400秒。元Searchの期限は1800秒のまま。30分のsilence expiry、時計、DB timestamp、旧campaignの13/12を変更していない。

このmeasurementはUNKNOWNへ到達せず終了した。最初のNative turnはSkillが要求するowner隔離確認を起動promptに見つけられず、typed `needs_input`を提出した。Search `search_36ebcf9479b6c5b359d9df8e685c3da4`は`unsatisfied / needs_input`で終端した。消費はSearch 1、割当round 1、exchange 1、inspection 0、Runtime attempt 0、経過51秒。新D、workload、transport切断、30分失効、UNKNOWN、receiptは未観測である。[公開証拠](evidence/formation-native-unknown-supplemental-20261005-01.json)。

元Searchをreopen/resetせず、別Searchを無断で作らない。承認済みSearch枠は消費済みであり、再試験には別measurementの明示承認が必要である。旧campaignは13/12超過で終了のまま。audit SHA-256 `91262a72bb27ebcc03107b7d0b6081135be78e6868e1d49f7ad5b70ef9470197`の不変を再確認した。独立API台帳、旧WBO UNKNOWN、過去の成功D・実測pinは変更していない。

原因を#1480の共通launcherで修正した。推論前に既存のOS負例を実際のCodex Code Mode model policyへ適用し、13項目・8クラスのcanaryのbool/digestをowner確認として渡す。欠落・一部だけの証拠はfail closed。Claudeではmodel policy fixtureとrestricted native tool configurationの範囲を区別する。新head `e03b5b2f5`のSkill関連43 testと実OS負例13項目はPASSしたが、このheadからの実Source探索は追加枠未承認のため未実施である。共通validatorは既存のClaude向けfrontmatter `disable-model-invocation`を未認識として拒否したため、そのvalidatorだけを根拠に形式を変えていない。

期限後のNative `reconcile-only`は保存されたUNKNOWNを報告するstatus/nextだけに限定し、報告windowを最大120秒・owner campaignのwall clock以内とする。元Searchの期限と消費枠は不変で、新しい探索input、submit、D、inspection、Runtime attemptは許可しない。null exchangeを新しい応答待ちと誤判定する条件も修正した。報告turnのSDK usageと内部LLM call/実請求は探索exchangeと別に記録する。この新経路の実UNKNOWN受入は未実施である。

開始guardはcampaignのwall clock予約とSearch期限を分けた。`--wall-clock-seconds 2400 --deadline-seconds 1800`は2400秒を予約する一方、Coordinatorへは1800秒だけを渡す。再接続のowner wall clockを保存済み期限と最初の予約から計算し、古いsnapshot、unanswered/cancelled round、reserved attemptが消費量を減らすことを認めない。guard/budget関連17 test PASS。今回の実台帳の最初の1800秒予約は、失敗後に2400秒へ書き換えていない。

今回の失敗Searchに新規UNKNOWN/attempt/resource reservationはない。active writerとquarantineは0。Native/Bridge、Runtime、Coordinator、proxy、SSH forwardを停止した。temporary owner sessionは通常signout APIのHTTP 200で失効し、remoteのprivate credential copy4件とlocal capability/auth link4件を削除した。SDKは累積tokenを報告したが、内部call数・実請求は不明であり、Ato direct LLM API call 0を推論費用0へ読み替えない。

Kuttの既存successful Search → Source Result → form-verify → State Run A/Bは別受入として#1481/API #731の証拠を維持する。今回の失敗をKutt成功で代替せず、#1485はDraft維持。stack merge、最終integration CI、staging/production、100 OSS、独立API残3計画はUNKNOWN gateの後に進める。
