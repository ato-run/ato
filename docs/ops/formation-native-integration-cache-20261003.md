# Formation統合SourceとCargo targetの照合

測定IDは `formation-native-integration-cache-20261003-01`。2026-10-03、macOS arm64、Rust 1.96.0。測定したclean Source pinはAto `8bb3a2b1dd1a3ab3b8e10da270ed9a627d53c0ec`。この文書追加commitへ測定pinを付け替えない。

統合Sourceには `functional_acceptance`、共通HTTP型の追加field、`legacy_exploration_supported` が存在し、同じworktreeのFormation crateを依存先としていた。Source差分を修正せず、統合worktree専用targetではRuntimeのHTTPテスト7件とWorker libテスト100件が成功した。

## targetの訂正と保存した失敗

途中の報告では、統合worktreeのdefault `$PWD/target` を既存の共有targetと呼んだ。これは誤りであり、次の3つを分けて記録する。

| 区分 | 実際のtarget |
|---|---|
| 元のWorker・CLI失敗 | `/Users/egamikohsuke/Ekoh/projects/ato-run/.tmp/formation-v0/ato/.tmp/target` |
| 今回のRuntime・Worker成功 | `/Users/egamikohsuke/Ekoh/projects/ato-run/.tmp/formation-agent-session/ato/target` |
| 独立した最小再現fixture | `/Users/egamikohsuke/Ekoh/projects/ato-run/.tmp/formation-agent-session/ato/.tmp/cargo-shared-target-probe/target` |

元の共有targetに対する `cargo test -p ato-formation-worker --lib` は10コンパイルエラー、exit 101。原stderrファイルは保存しておらず、tool outputから転記した `.tmp/integration-final/initial-worker-tool-observation.json` が証拠である。その後の共有targetに対する `cargo test -p ato-cli --lib` も同じ10エラー、exit 101で、こちらは `.tmp/integration-final/cli-lib.log` を保存した。いずれも実際に消費したFormation artifactのID・fingerprintは不明。

今回の `cargo clean -p ato-formation` は統合worktree専用targetにのみ適用した。元の共有targetはcleanしておらず、この記録は元の共有targetの修復・成功を示さない。既存の過去測定記録は変更していない。

## 統合Sourceの再build

`--verbose` のrustc出力で `--out-dir`、依存artifact、incremental directoryが統合worktree専用targetにあることを確認した。

| コマンド | 結果・ログ |
|---|---|
| `cargo clean -p ato-formation` | 専用targetのFormation artifactのみ削除。`.tmp/integration-formation-package-clean.log` |
| `cargo test -p ato-runtime-attempt --lib port_operations --verbose` | 7 PASS。Formationを当該Sourceからcompile。`.tmp/runtime-attempt-integration-after-clean.log` |
| `cargo test -p ato-formation-worker --lib --verbose` | 100 PASS。Formationを当該Sourceからcompile。`.tmp/worker-integration-after-clean.log` |

この2件のテストには専用target内のincremental directory指定があり、後述する別の統合測定の `CARGO_INCREMENTAL=0` と混同しない。

親担当は同じclean Source pinと専用targetを用い、`CARGO_INCREMENTAL=0` でCLI lib 49件、MCP mode境界1件、Activity MCP 1件、CLI/Worker all-target strict clippy、workspace fmt、MCP buildを成功させた。この結果は別測定 `formation-agent-integration-local-20261003-01`、`.tmp/integration-final/integration-results.json` に属する。

## 独立した最小再現

同じpackage名・version・相対配置を持つbase/headの小さなRust workspaceを作り、fixture専用のtargetを共有した。baseの依存crateには旧関数、headには追加関数を置き、全fixture Sourceのmtimeをbase buildより前へ設定した。

1. baseの `cargo check --verbose` は旧依存をcompileした。
2. headのcheckは依存・呼び出し側の両方を `Fresh` とし、base artifactを再利用した。
3. headの呼び出し側Sourceだけmtimeを更新すると、依存は `Fresh` のまま呼び出し側をcompileし、追加関数が見つからずE0425・exit 101となった。
4. fixture target内の依存packageだけcleanすると、headの依存Sourceをcompileし、同じcheckが成功した。

これにより、別worktreeで同じtargetを共有した際の古いpath依存artifact再利用という条件を再現した。元の失敗時に使われた個別artifactを特定した証拠ではない。fixtureのSource bytes、手順、target、各ログhashを [機械記録](evidence/formation-native-integration-cache-20261003.json) に保存した。

新たな実Codex/Claude Codeの探索、PWAからRuntimeへの入力resume、OSSの機能・保存・再起動は実施していない。独立有料推論API call、実受入Runtime attemptは0。Search/K/D、UNKNOWN、既存receipt・台帳の変更、配備、remote migration、flag変更、通常Run許可も行っていない。
