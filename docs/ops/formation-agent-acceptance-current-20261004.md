# Formation Agent Session受入 — 2026-10-04

Session共通化 #1479 → 共通Skill #1480 → この受入記録の順でレビューする。入力・状態連携はAPI #731 / Ato #1481 / PWA #418の別単位。配備・remote migration・通常Run許可・100件再測定は実施していない。

実Codex 0.160.0 / gpt-6.1-solは、新しい文脈・既知DなしのSVGOMG Sourceでnpm lifecycle失敗を修復し、固定Kのfresh PASSへ到達した。3 exchange、2 D round、2 attempt。Runtimeの保存済みACKは両attemptでaccepted/closed、予約は0。成功は `k_reached_awaiting_assessment`。取得できたSDK token数は下記に記録し、内部LLM call数と実請求額はunknownを維持する。Atoからの直接推論API callが0でも推論費用0とは扱わない。

実Claude Code 2.1.288 / claude-opus-5-5も、同じSource/K・初期情報・上限の別Search、新しい文脈、Codex成功Dや詳細報告なしでSVGOMGを成立させた。初回npm lifecycle失敗→同じround内のnative toolchain validation修正→fresh PASS。3 exchange、2 D round、2 Runtime attempt。保存後の同一再送を受理、異内容・古いdigest・未知exchangeを拒否し、実Native clientを切断した後、同じSearchへ新しいNative文脈で接続した。保存済みr1を再推論・再実行せず、元deadlineと残attempt1を維持した。両Runtime ACK accepted/closed、全予約0、変数要求/assignment/metadata/encrypted値0。成功は危険性評価待ちのまま。[Claude実Source証拠](evidence/formation-native-claude-source-20261004.json)。以前のquota Searchは取消済みで再開していない。

今回のunique app成功はCodex 1、Claude Code 1、独立API 0。両Nativeは初回PASS 0、失敗後修正PASS 1で、同じSVGOMGなので合算unique appは1。Kutt機能fixtureを加算しない。Claudeの全SDK tool出力/Native stderrで、private生成email、Coordinator credential、RAMだけで照合したNative provider credentialのcanary漏出0。SDK報告値と実請求額を区別する。Claude測定launcherはintegration `e021eecb`、Codex測定launcherは `754bdb57`。その後のmodel binding guard・SDK報告処理・文書の変更は、成功pinへ付け替えない。実Nativeの停止ケースと入口検査をSource成功と分け、未確認を完了と扱わない。

停止ケースを追加した。実Claude Codeの新文脈で、値や成功Dなしの制御Node Sourceを読み、未提供のowner入力に対して共通typed declineを提出し、`needs_input`で停止した。1 exchange/1 round/Runtime attempt 0。これはD提案前の不足入力辞退であり、実PWAで確認したRuntime変数pause/resumeとは別。expanded budget 0のケースはCoordinatorがSource admissionを409 `search_budget_exhausted`で拒否し、Search/Bridge/Native文脈/実行を作らなかった。10秒の元deadlineケースはRequester/authorityが`deadline_exceeded`で停止し、後から渡したNative入口も推論前に拒否した。予算拒否・期限拒否では実モデルを起動していない。[停止証拠](evidence/formation-native-stop-20261004.json)。

事前集計に未回答のquota取消roundを含めていなかった誤りがある。Coordinatorが割り当てたroundは13で、計画12を1超過した。応答完成済み11だけを消費総数と扱わない。exchange 15/28、Runtime attempt 7/8、inspection 0/16、時間1368/7200秒、identifierは未作成2を含め10。最後の期限ケースによる追加推論・実行は0だが、超過を保持して追加Native Searchを停止した。元のSearch上限・API台帳・旧結果は変更していない。[集計証拠](evidence/formation-native-budget-audit-20261004.json)。

Owner受入の開始前集計を、既存CLIのstatusと保存済みviewを読む薄い `scripts/acceptance/coverage/native-session-budget.py` へ分離した。取消・未回答・予約attemptも数え、再接続や古いsnapshotで消費量を減らさない。binding変更・不足証拠はfail closed。関連4test PASS、実台帳でも追加開始を拒否した。実SDKによるSource探索/復旧と、厳密fixtureだけで確認したUNKNOWN切断などを分け、完全受入のDoneとはしない。

SDKの保存記録から取得できたtoken数を補う。Codexの最終SDK累計はinput 554,685（cached 489,984）/output 3,555。15回のusage updateを15 LLM callとは数えない。Claudeの再接続文脈はSDK list価格推計$0.6573648、入力不足の制御文脈は$0.2069404。最初の切断文脈は累計result未取得でpartial。SDK推計はaccountへの実請求ではなく、DeepSeek台帳へ合算しない。内部LLM call数と実請求額はunknownを維持する。[SDK報告値](evidence/formation-native-sdk-reported-usage-20261004.json)。

測定後のlauncher integration `79ee6e47` はSDK報告値の抽出・終了出力だけを追加し、`59aa25e8` はSDK counterの形式変更や不正な数値でも保存済み結果を報告できるようにした。Skill PRの対応headは `af3b2f20`。現在のNative/Skill testは39 PASS。Claude累計resultをturnごとに足さず、未知fieldをコピーせず、欠損を費用0にしない。保存済みSDK streamの読取と単体検証で確認し、Source/Runtime受入を新headで再実行したとは報告しない。

現在のNative入口へ終了済みCodex/Claude Searchを渡す負例は、両方とも新しいNative文脈・推論・Runtime実行の作成前に停止した。全保存済みJSON/journalのSHAは不変、Search/exchange/attempt追加は0。これは実Native entryのadmission検証で、実LLMによるneeds_input/UNKNOWN停止とは分ける。[終了済みSearchの入口証拠](evidence/formation-native-terminal-entry-20261004.json)。

[Codex実Source証拠](evidence/formation-native-codex-source-20261004.json)、[OS別CI証拠](evidence/formation-native-platform-ci-20261004.json)。Session `8bd25250` のUbuntu/macOS/Windows実CLI CIはPASS。WindowsのMSVC実行stepとjournal検査を記録した。旧Linux WebSocket・macOS worker失敗の原因は未再現であり、現headのgreenを旧原因の解明へ読み替えない。

独立APIのSVGOMGは同じSource/K・既知Dなしで実行し、3 call、1 inspection、2 D round、2 attemptでexhausted。HTTP権限不足を修正した後、npm lifecycle計画不足で停止しPASSは0。準備済み4計画は同じaccount上限を共有する。累計21/41 call、小規模4/24、残20、未精算0、ピーク価格による推計残予算$0.467188。provider実請求額はunknown。[独立API証拠](evidence/formation-independent-api-svg-20261004.json)。

実PWA入力と権限変更後のresumeは、full API、実Chromium、実contained Runtimeで測定した。ローカルowner認証のbootstrapとSourceは制御fixtureで、外部account・Kutt・独立Agent成功数に含めない。保存・reload・取消・revoked停止の後、PWAから新しいprivate値を入力し、元のattempt `01M41FPT80P1EA34PYDP7PCXY5` が元deadline・枠のままfresh PASS、ACK、停止、cleanupまで到達した。有効metadata・encrypted値は0。PWAの確認済み登録nonceはACK後に削除し、新しい入力へ古いnonceを流用しない。応答消失時は同じnonceを保持する。

測定PWA pinは `b4e2c3b`、後続 `3c8ec7aa` の差分はharness診断の漏出抑止、PR head `8607717` は受入文書の更新で、製品input componentは同じblob。失敗したharness測定も保持した。[PWA/Runtime証拠](evidence/formation-product-input-runtime-20261004.json)。canary検査は今回の生成値と対象出力に限る。旧測定の固定emailなどの制約を遡って修正しない。

Kutt新artifact `38fb9315…` は、既存Rust authority・実Runtime・StateServiceによる別の機能受入を完了した。2つの新規Runで元と同じK/Dのfresh PASS、ACK、停止、予約解放、入力cleanupを確認した。1回目はログイン・リンク作成・一覧・外部へ追従しないリダイレクト観測、2回目は新Runでの保存revision復元・ログイン・リンク削除・旧JWT拒否を確認した。writer fenceは1→2、active writerとencrypted値は最終0。既知Dを再利用した機能受入であり、独立Agent/APIのSource探索成功には数えない。[Kutt機能・状態証拠](evidence/formation-kutt-current-functional-state-20261004.json)。

このKutt測定は明示したprivate StateService dispatch namespaceのfixtureで、通常Source経路からの自動state provisioningは未完了。元のCLI Source SearchにはSource Instanceがなく、現製品APIの登録には同じclosureのFormationResult/既存sealed namespaceが必要。仮のInstanceやCapsuleRefを挿入して製品経路の成立を主張しない。過去の別artifactの成功とは分けた。検証用email/passwordはprivate生成し、公開Owner報告で漏出0を確認した。この機能fixtureに実エージェントのtool transcriptはなく、Runtime内のprivate DB/stateを公開証跡として検査したとは主張しない。

Owner調査では、終了済みCodex Source Searchの接続descriptorを広く読んだ際に、限定Bridge access tokenを一度tool出力へ表示した。値はこの公開記録へ保存しない。対象Requesterの終了とBridgeの待受なしを確認した。表示対象は当該SearchのBridge接続権限で、Coordinator owner credential・Runtime ticket・provider keyは含まれていない。上記のcanary漏出0は検査したNative SDK tool streamや指定Owner報告の範囲に限り、この調査中の表示を含めた全操作の漏出0とは主張しない。

実測API pin `7fae389b` は、base `353c06fe` の最小Worker起動で再現したschema循環importを修正している。registration/projection validatorを副作用のない共通leafへ移し、同じNode 22.14・entryでheadの起動を確認した。schema・Source closure guardは変更していない。typecheckと既存retained関連2testはPASS。API #731の同じ修正は `a27397ee`。修正前の起動失敗と、canonical D登録前の既知Dによる未実行失敗は保全し、後者は同じattemptの保存済みadmission refusalで精算・writer解放した。再推論・再実行や旧Source Searchの再開はしていない。

独立APIの残計画は、鍵の提供後も実行前条件を満たしていない。readonly CLI snapshotで旧18 cellのjournal不変と新SVG 3 cell、合計21/41・未精算0を再確認した。準備済みOCI/Python/Kuttの6/6/8上限は残20を共有するが、計画は旧18 cellと異なるbinary pinのまま。remote diskの20 GiB下限を除く余裕は663,830,528 byteで、宣言されたexpanded 2 GiB + stored 1 GiBを確保できない。OCI builderも未bind。追加callやSearchを開始せず、元計画・予算・旧証拠を保持した。[API実行前照合](evidence/formation-independent-api-readiness-20261004.json)。自分の検証用Coordinator/Runtime/supervisorとSSH forwardは終了し、全7 attemptのaccepted結果とclosed journalを保持した。共有serviceや配備・migrationは変更していない。
