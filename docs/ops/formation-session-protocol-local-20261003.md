# Formation共通Sessionのローカル検証

測定ID: `formation-session-protocol-local-20261003-01`。2026-10-03、macOS arm64、Rust 1.96.0。

実装pinはAto `6985e0ef23b1dfb203f68d78e7e74dbefeeb0aac`（clean tree）。baseはDraft #1476の`31a6d6b4d6df03a7463058ea38f87b7ff1e525a3`。この記録の追加commitへ、実行pinを付け替えない。

## 実装と検証

`agent_session`はCodex/Claude Codeの種別、製品version、取得できたmodelを固定する。旧`codex_session`のconfig bytes/digestを維持する。共通Rust validator、Requester、Coordinator、Runtimeの責務はそのまま使う。

Ownerの`ato form --session-bridge PATH`は、同じSource/K、Requester journal、Search、config、agent、元deadlineを使う。Owner journalのexclusive lock、immutable exchange window、input digest、atomic response publicationにより、再接続・再取得・同一応答再送でexchangeやattemptの枠を補充しない。取消要求はAPI応答より先に保存し、取消ACKを失っても新たな提案を拒否する。

Producerの入口は`ato form-session`と`ato-formation-session-mcp --connection PATH`。MCPの固定4操作は既存Sessionへ転送し、任意path/URL/Searchを引数に受けない。型付きJSONを文字列→RawValueで渡してduplicate fieldを共通validatorへ残す。公開viewはprivate input、ticket、grant、raw receipt/HTTP bodyを掲載せず、ACK不明を完了扱いにしない。

| コマンド | 結果 |
|---|---|
| `cargo test -p ato-formation-worker --lib runtime_network::proposal::reasoning::session` | 9 PASS。digest/agent互換、再取得、saved response、古い応答、同時提出、二重Owner、UNKNOWN、取消ACK消失、元deadline/start clock、公開viewのprivate値不在。 |
| `cargo test -p ato-formation-worker --lib` | 89 PASS。既存独立API transport、provider予算、旧Session inspection/recoveryのfixtureも含む。 |
| `cargo test -p ato-cli --lib` | 36 PASS。固定MCP inventory、任意path拒否、再接続binding、RawValue duplicate、stdioのサイズ/エラー境界。 |
| `cargo test -p ato-cli --test activity_mcp` | 1 PASS。既存Activityの大きなescaped metadataを含む互換性。 |
| `cargo clippy -p ato-cli -p ato-formation-worker --all-targets -- -D warnings` | PASS。 |
| Legacy Python helperのatomic publication smoke | inspection/decline/proposalの3transport payloadで同一再送を受理、異内容を拒否。duplicate JSON field拒否。これはinner proposalのRuntime受入ではない。 |
| `git diff --check` | PASS。 |
| `cargo fmt --all -- --check` | base由来の3ファイルのみFAIL。Session差分のformatは一致。修正は独立CI PR #1478の予算formatter変更。 |

一時出力は当該worktreeの`.tmp/`、Cargo cacheはworkspace内の既存`.tmp/formation-v0/ato/.tmp/target`を使用した。テスト対象Sourceは当該clean worktreeであり、別worktreeのSourceを混入していない。

## 未測定と依存

実Codex/Claude Codeのnative Skill明示呼び出し、LLM探索、RuntimeのOSS実行、PWA入力resume、機能/保存/再起動はこの測定に含まれない。fixtureにagent名を設定した結果を実エージェント対応の完了として扱わない。whole-process/tool隔離と製品version/authの受入も別gateである。

API取消入口はDraft ato-api #730、CI portability/formatterはAto #1478、共通Skillは後続レビュー単位。Skillの配置・実探索計画はその単位で管理する。API cancellationのremote migrationは未適用。

この測定による推論、独立有料API call、Runtime attemptは0。旧測定pin、失敗Search、UNKNOWN、receipt、台帳へ変更なし。マージ、配備、flag変更、通常Run許可も行っていない。
