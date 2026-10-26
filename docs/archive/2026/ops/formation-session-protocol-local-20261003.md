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

## 入力待ちの限定メタデータ（別測定）

測定ID `formation-session-protocol-local-20261003-02`、実装pin `02c331ddef0336ee90a1c0ae6de1a4ca211d002d`のSource bytesで実施。上の測定01へ結果を追加・付け替えない。

`needs_input`の公開viewは、共通Rust validatorで検査した変数名・型・resource・operation・phase・secret/embedding/temporaryフラグだけを返す。purpose、account、endpoint、tenant、service、obtain/error、grant候補は掲載しない。不正な要件も掲載しない。入力待ち中は推論入力と応答提出を閉じ、消費済みexchangeを維持する。

Session専用テスト10件PASS。非公開値のcanaryと不正要件を含む新しい入力待ちテストを追加した。Worker all-target strict clippyも同じ変更bytesでPASS。実製品、PWA、Runtimeの受入は引き続き未測定。

## 期限後の保存済み応答照合（別測定）

測定ID `formation-session-protocol-local-20261003-03`。応答保存のACK直後に
deadlineを跨ぐと、Ownerが未照合応答を残したまま終わる境界を検査した。
実装pin `69b50256cd6dee7b71b19147fcef425055879d3e` のSourceでSession専用
15件とWorker all-target strict clippyがPASS。

原子的に保存済みの応答をdigest/exchange/windowへ照合し、期限後の受領証拠だけを
一度記録する。`session_closed_response`は元のexchange deadline、admission deadline、
観測時刻を保存する。新しい入力、inspection、Dの実行、予算補充を許可しない。
公開viewは `closed_response_reconciled` とOwner照合の次操作を返す。
Requesterはこの経路をtimeoutへ分類し、provider infrastructure failureへ分類しない。
window欠損・応答の改変・古い応答・競合・繰返し照合は拒否または冪等として検査した。
旧recordは新fieldを省略し、旧bytes/digestの意味を維持する。

## Session公開入力の限定投影（別測定）

測定ID `formation-session-protocol-local-20261003-04`。実装pin
`d6fe62331959adfbb6c21637921ed0d7e5a2c345` のSourceでSession専用18件と
Worker all-target strict clippyがPASS。API-validなowner metadataにprivate canaryを
含め、owner保存bytes/digestを維持したまま、両agentへ同じ限定投影を返すことを確認した。

変数は完全なmetadata envelopeと共通Rust要件validator、Source closure、Search scopeを
検査してtyped slot・reuse・期限・取消だけを公開する。purpose/service/endpoint/account/
tenantやapplication/scopeの列挙は公開しない。Runtime情報もtyped operation catalogから
決まるfact keyと値を許可し、任意prefix・host path・自由記述healthを公開しない。
不正・未知・scope不一致のmetadataは省略する。

viewの `input_projection_schema = ato.formation-session-public-input/1` と
`input_sha256_scope = owner_saved_input` は、公開投影と元の保存入力を区別する。
Producerは返されたdigestをechoし、公開投影の再hashで置き換えない。
Owner保存入力と独立API transportの既存契約は変更しない。このfixtureは全API経路や
実Native製品を通したprivate値の完全非露出を証明しない。

外部Producer relayの測定は別の
[記録](formation-session-producer-relay-20261003.md)とIDを持つ。上の01〜04へ
relayの結果や実Runtime成功を付け替えない。旧報告の固定非secret email露出の制約も
修正していない。
