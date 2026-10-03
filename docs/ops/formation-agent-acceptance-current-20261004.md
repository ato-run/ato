# Formation Agent Session受入 — 2026-10-04

Session共通化 #1479 → 共通Skill #1480 → この受入記録の順でレビューする。入力・状態連携はAPI #731 / Ato #1481 / PWA #418の別単位。配備・remote migration・通常Run許可・100件再測定は実施していない。

実Codex 0.160.0 / gpt-6.1-solは、新しい文脈・既知DなしのSVGOMG Sourceでnpm lifecycle失敗を修復し、固定Kのfresh PASSへ到達した。3 exchange、2 D round、2 attempt。Runtimeの保存済みACKは両attemptでaccepted/closed、予約は0。成功は `k_reached_awaiting_assessment`。内部LLM call・token・費用はunknownであり、Atoからの直接推論API callが0でも推論費用0とは扱わない。

実Claude Code 2.1.288 / claude-opus-5-5は独立Searchと実Skill起動まで確認したが、利用枠超過で推論が停止した。旧Searchはowner取消済み。復帰後の新規Search検証は未実施。Codex成功Dや詳細報告を新文脈へ渡さない。過去の固定応答fixtureを実ClaudeのSource成功に数えない。ネイティブ受入の残項目があるためDoneとはしない。

[Codex実Source証拠](evidence/formation-native-codex-source-20261004.json)、[OS別CI証拠](evidence/formation-native-platform-ci-20261004.json)。Session `8bd25250` のUbuntu/macOS/Windows実CLI CIはPASS。WindowsのMSVC実行stepとjournal検査を記録した。旧Linux WebSocket・macOS worker失敗の原因は未再現であり、現headのgreenを旧原因の解明へ読み替えない。

独立APIのSVGOMGは同じSource/K・既知Dなしで実行し、3 call、1 inspection、2 D round、2 attemptでexhausted。HTTP権限不足を修正した後、npm lifecycle計画不足で停止しPASSは0。準備済み4計画は同じaccount上限を共有する。累計21/41 call、小規模4/24、残20、未精算0、ピーク価格による推計残予算$0.467188。provider実請求額はunknown。[独立API証拠](evidence/formation-independent-api-svg-20261004.json)。

実PWA入力と権限変更後のresumeは、full API、実Chromium、実contained Runtimeで測定した。ローカルowner認証のbootstrapとSourceは制御fixtureで、外部account・Kutt・独立Agent成功数に含めない。保存・reload・取消・revoked停止の後、PWAから新しいprivate値を入力し、元のattempt `01M41FPT80P1EA34PYDP7PCXY5` が元deadline・枠のままfresh PASS、ACK、停止、cleanupまで到達した。有効metadata・encrypted値は0。PWAの確認済み登録nonceはACK後に削除し、新しい入力へ古いnonceを流用しない。応答消失時は同じnonceを保持する。

測定PWA pinは `b4e2c3b`、最終 `3c8ec7aa` の差分はharness診断の漏出抑止だけで、製品input componentは同じblob。失敗したharness測定も保持した。[PWA/Runtime証拠](evidence/formation-product-input-runtime-20261004.json)。canary検査は今回の生成値と対象出力に限る。旧測定の固定emailなどの制約を遡って修正しない。

Kutt新artifact `38fb9315…` の機能・状態保存・新Run復元と、通常Source経路からの自動state provisioningは未完了。元のCLI Source SearchにはSource Instanceがなく、現製品APIの登録には同じclosureのFormationResult/既存sealed namespaceが必要。仮のInstanceやCapsuleRefを挿入して製品経路の成立を主張しない。旧private fixtureの別D/artifactによる成功はこの新artifactの証拠にならない。
