# 実UNKNOWN専用の追加受入

ユーザーが40分案を承認したため、`formation-native-real-unknown-20261005-01`を実Codex 0.160.0 / gpt-6.1-solで実行した。上限はSearch 1 / D round 2 / exchange 3 / Runtime attempt 1 / inspection 2 / wall clock 2400秒。元Searchの期限は1800秒のまま。30分のsilence expiry、時計、DB timestamp、旧campaignの13/12を変更していない。

このmeasurementはUNKNOWNへ到達せず終了した。最初のNative turnはSkillが要求するowner隔離確認を起動promptに見つけられず、typed `needs_input`を提出した。Search `search_36ebcf9479b6c5b359d9df8e685c3da4`は`unsatisfied / needs_input`で終端した。消費はSearch 1、割当round 1、exchange 1、inspection 0、Runtime attempt 0、経過51秒。新D、workload、transport切断、30分失効、UNKNOWN、receiptは未観測である。[公開証拠](evidence/formation-native-unknown-supplemental-20261005-01.json)。

元Searchをreopen/resetせず、別Searchを無断で作らない。承認済みSearch枠は消費済みであり、再試験には別measurementの明示承認が必要である。旧campaignは13/12超過で終了のまま。audit SHA-256 `91262a72bb27ebcc03107b7d0b6081135be78e6868e1d49f7ad5b70ef9470197`の不変を再確認した。独立API台帳、旧WBO UNKNOWN、過去の成功D・実測pinは変更していない。

原因を#1480の共通launcherで修正した。推論前に既存のOS負例を実際のCodex Code Mode model policyへ適用し、13項目・8クラスのcanaryのbool/digestをowner確認として渡す。欠落・一部だけの証拠はfail closed。Claudeではmodel policy fixtureとrestricted native tool configurationの範囲を区別する。新head `e03b5b2f5`のSkill関連43 testと実OS負例13項目はPASSしたが、このheadからの実Source探索は追加枠未承認のため未実施である。共通validatorは既存のClaude向けfrontmatter `disable-model-invocation`を未認識として拒否したため、そのvalidatorだけを根拠に形式を変えていない。

期限後のNative `reconcile-only`は保存されたUNKNOWNを報告するstatus/nextだけに限定し、報告windowを最大120秒・owner campaignのwall clock以内とする。元Searchの期限と消費枠は不変で、新しい探索input、submit、D、inspection、Runtime attemptは許可しない。null exchangeを新しい応答待ちと誤判定する条件も修正した。報告turnのSDK usageと内部LLM call/実請求は探索exchangeと別に記録する。この新経路の実UNKNOWN受入は未実施である。

開始guardはcampaignのwall clock予約とSearch期限を分けた。`--wall-clock-seconds 2400 --deadline-seconds 1800`は2400秒を予約する一方、Coordinatorへは1800秒だけを渡す。再接続のowner wall clockを保存済み期限と最初の予約から計算し、古いsnapshot、unanswered/cancelled round、reserved attemptが消費量を減らすことを認めない。guard/budget関連17 test PASS。今回の実台帳の最初の1800秒予約は、失敗後に2400秒へ書き換えていない。

今回の失敗Searchに新規UNKNOWN/attempt/resource reservationはない。active writerとquarantineは0。Native/Bridge、Runtime、Coordinator、proxy、SSH forwardを停止した。temporary owner sessionは通常signout APIのHTTP 200で失効し、remoteのprivate credential copy4件とlocal capability/auth link4件を削除した。SDKは累積tokenを報告したが、内部call数・実請求は不明であり、Ato direct LLM API call 0を推論費用0へ読み替えない。

Kuttの既存successful Search → Source Result → form-verify → State Run A/Bは別受入として#1481/API #731の証拠を維持する。今回の失敗をKutt成功で代替せず、#1485はDraft維持。stack merge、最終integration CI、staging/production、100 OSS、独立API残3計画はUNKNOWN gateの後に進める。

## 追加承認されたmeasurement 02

ユーザーの「承認します。続けて」を受け、同じ上限の別measurement `formation-native-real-unknown-20261005-02`を開始した。開始前のCLIはscratch不足で停止したが、コピー元DBと受入DBのSearch/request/attempt件数がすべて6/6/7で一致し、新Search未作成を確認した。同じ予約を保持し、再生成可能なbuild outputを整理して、予約済み開始処理を継続した。この失敗launchも台帳に保存した。旧13/12、measurement 01、既存Search、期限、消費量は書き換えていない。

修正済みSkill `e03b5b2f5`を実Codex 0.160.0 / gpt-6.1-solから呼び出し、制御Sourceを既知Dなしで探索した。Search `search_36277e87df76feb9bb5e444fa116307a`、attempt `01M45DWSE3GWKKDECQ3P3Q6VNY`で固定Kのfresh PASS、結果保存、ACK、closed、candidate stop、cleanupを確認した。消費はSearch 1 / D round 1 / exchange 1 / Runtime attempt 1 / inspection 0 / 58秒。通常Run許可、functional registration、公開、forkへは昇格していない。[公開証拠](evidence/formation-native-unknown-supplemental-20261005-02.json)。

ただしUNKNOWNゲートは未達である。切断ハーネスの監視はargvにSourceファイル名`server-unknown.js`が現れることを前提としていた。materialized entrypointを検出できず、開始markerがない場合にproxyが結果を通す構造だったため、結果はCoordinatorへ届いた。実測PASSをUNKNOWNへ変更せず、DBや時計を変更して再分類しない。30分silence expiryと新Native文脈でのUNKNOWN照合は今回も未観測である。

この欠陥に対し、owner側の受入専用monitorは固定RuntimeのPID/start ticksと子孫関係、固定Node executableのbytesを照合する。mount namespace内のパスやSourceファイル名には依存せず、env/argvを収集しない。受入専用proxyは最初のresult配送で対象RuntimeのPID/start ticks/argv/binary hashを照合して停止し、元のdispatch/retryを保存する。開始証拠が欠落・identityが不一致でも、明示的owner releaseまでresultを転送しない。開始証拠が欠けた場合は受入成功にしない。これらは測定用fault injectionであり、Coordinatorの30分失効や製品のSearch budgetを変更しない。

Linuxでproxy 4 testとmonitor 7 testがPASSした。開始markerの有無、identity不一致での配送拒否、owner release後の配送、実fixture processへのSIGSTOP/SIGCONT、実Node子プロセスの観測、materialized file ID、namespace内のパス差、PID再利用、無関係/終了済みprocessを確認した。fixtureは新Source Search、Formation Runtime attempt、Native推論を作らず、実UNKNOWN受入とは区別する。新measurement 03の40分計画は準備したが、追加Searchを未承認で作っていない。

measurement 02の最終attempt/resource reservation、active writer、quarantine、temporary variable値、live metadataは0。Native/Bridge、Runtime、Coordinator、proxy、watcher、SSH forwardを停止した。owner sessionは通常signoutのHTTP 200で失効し、remoteのcredential copy4件とlocal capability/auth link4件を削除した。保存されたmodel向けイベントのowner credential/capability値14 encodingの一致は0。SDK累積tokens 69,496（input 68,769、cached input 50,816、output 727）を別記録とし、実call数・請求は不明のまま。旧13/12 auditのhash不変を再確認した。

## 追加承認されたmeasurement 03

ユーザーの追加承認で`formation-native-real-unknown-20261005-03`を実行した。Search 1 / D round 2 / exchange 3 / Runtime attempt 1 / inspection 2 / wall clock 40分を維持し、旧13/12とmeasurement 01/02は変更していない。実Codex 0.160.0 / gpt-6.1-solは既知Dなしの制御SourceからDを1回提案した。実Runtimeの保存済みfresh receiptによりworkload開始を確認し、result transportを保持した。独立した`/proc` monitorの開始markerは今回も取得できず、最初の切断のmarker欠落を保存した後、同じRuntime/result dispatchの通信retryを開始確認後に再度停止した。独立process観測が成功したとは報告しない。新しいRuntime attemptは作っていない。

Search `search_b44da962307739556f9121288edf1c04`、attempt `01M45JR7BASXDRFDXF8W1CC6YE`は08:26:43.791 UTCにclaimされ、08:56:51.571 UTCに実際のUNKNOWNへ到達した。claimから1,808,017msであり、既存30分失効、時計、DB timestampを変更していない。元NativeはUNKNOWNまで待機状態で保持し、その後終了した。待機中とUNKNOWN後のowner viewではround 1、exchange 1、attempt used 1、元deadline、`input: null`が不変だった。byte予約はUNKNOWN時に保守的な消費へ移り、消費済み量を返却していない。[公開証拠](evidence/formation-native-unknown-supplemental-20261005-03.json)。

新しいNative文脈を同Searchへ`reconcile-only`で起動しようとしたが、MCP brokerが期限後のrelayを拒否した。Native SDK contextの開始前に`fixed_mcp_broker_unavailable`で停止し、新しい推論turnは0だった。launcherだけには最大120秒の報告windowがあった一方、Rust relayは元Search deadlineを超えられず、共通入口の実装不整合が露呈した。この実測はゲート未達であり、#1485をReadyにしていない。40分枠を延長せず、追加Searchやattemptで代用していない。

探索枠終了後の09:06:12.602 UTCに、ownerが保存済み結果の配送を解放した。元result bytesのSHA-256は`f572631646f9261170227ac6389cb9b09d4e7d664eb653545880302435a9bf86`で不変。既存のretry 2により同一attemptのlate evidenceを受理し、ACK `accepted: true`、delivery `closed: true`を確認した。ADR-028の現契約ではlate PASSはUNKNOWNをPASSへ変更しない。Runtime終了、candidateのdisposable realization破棄、cleanup成功を確認し、ownerの通常resolution APIで`effect_reconciled`を保存した。履歴attemptはUNKNOWNのまま、元Searchは`unsatisfied`で終端した。手作業のDB修正、別attempt、通常Run許可、functional registrationは使っていない。

最終状態は未解決UNKNOWN、Search予約、active writer、quarantine、一時変数値、live metadataがすべて0。owner sessionは通常signoutで失効し、remote secret copy4件とlocal capability/auth link4件を削除した。Native/Bridge、Runtime、Coordinator、proxy、monitor、bootstrapを停止し、local forwarding portのlistener不在を確認した。履歴ticketの予約列は監査用に保持されるため、未精算予約はauthorityのSearch budgetで確認した。SDK累積tokens 68,898（input 68,247、cached input 41,472、output 651）は実call・請求と分け、両者は不明とした。秘密値の削除前に全encoding照合を保存できなかったため、measurement 03では「全操作で非露出」と主張しない。起動時のOS負例13項目・8クラスの成功と、cleanupの0件確認を別証拠として扱う。

不整合は#1479のRust relayで修正した。ownerが明示する`--reconcile-only`は、保存済みUNKNOWN、未解決attempt、`input: null`、同じSearch/configurationを必須とし、最大120秒の独立した報告期限だけを認める。signed bindingにread-only scopeを含め、TCP/Unixの共通dispatchで`status / next`だけを公開する。`submit / cancel`の直接呼び出し、scope改変、入力待ちへの変化を拒否する。通常relayの期限制限は元Search deadlineのまま。#1480の共通launcherはこのscopeと報告期限をbrokerへ渡す。MCP 28 test、起動引数1 test、Skill 44 test、strict clippy all-targets/all-featuresがPASSした。

修正後の実UNKNOWNゲートは未実施。measurement 03のSearch/40分枠は消費済みなので、新たな実測は別measurementの明示承認後に行う。今回の実測pinを修正後headへ付け替えず、stack merge、最終integration CI、staging/production、100 OSS、独立API残3計画は引き続きこのゲートの後とする。
