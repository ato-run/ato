# Formation共通Session・状態経路の統合検証

測定ID `formation-agent-integration-local-20261003-01`。Rust Source pinはAto
`8bb3a2b1dd1a3ab3b8e10da270ed9a627d53c0ec`。macOS arm64、Rust 1.96.0。
Session、外部relay、共通状態/機能受入、CI portability変更を統合したSourceを検査した。
実Codex/Claude Codeの探索や実OSS受入を行った記録ではない。

## 統合Sourceとartifact

CLIとMCP binaryの検証時はclean tree。Portable Application追加検証中に文書の準備が
あったが、tracked Rust Sourceは同じpinのbytesのまま。後続の文書commitへ実行pinを
付け替えない。targetは当該worktree専用の `$PWD/target`、TMPDIRは当該`.tmp`、
以下のコマンドは `--locked --offline`、`CARGO_INCREMENTAL=0` を固定した。

| 検証 | 結果 |
|---|---|
| `cargo test -p ato-cli --lib` | 49 PASS。Session/MCP/relayの競合・再送・失効・非露出境界を含む。 |
| `cargo test -p ato-cli --bin ato-formation-session-mcp` | 1 PASS。Owner publication/Producer relayのmode競合。 |
| `cargo test -p ato-cli --test activity_mcp` | 1 PASS。既存Activity stdio互換。 |
| `cargo test -p ato-cli --test portable_application` | 15 PASS。local processの所有、停止/再起動、immutable bundle/receiptを含む。 |
| `cargo clippy -p ato-cli -p ato-formation-worker --all-targets -- -D warnings` | PASS。 |
| `cargo fmt --all -- --check` | PASS。CI #1478の既存format修正を含む。 |
| `cargo build -p ato-cli --bin ato-formation-session-mcp` | PASS。 |

同じSourceのWorker 100件、Runtime HTTP 7件は別測定
[`formation-native-integration-cache-20261003-01`](formation-native-integration-cache-20261003.md)
に属する。専用targetの成功と元共有targetのコンパイル失敗を区別し、成功へ
書き換えない。この検証のためのSource修正は0。

固定したMCP artifactは6,092,024 bytes、SHA-256
`0aca096859d4f4410d3674c0ef061bd572762774b680995d423f2c8d619a62f8`。
専用測定領域へcopyしmode0555で保存した。以前のrelay単独artifactのhashを、この統合
artifactの受入へ代用しない。全コマンドのlog hashとartifact pathは
[機械記録](evidence/formation-agent-integration-local-20261003.json)に保存した。

## 新しいGitHub CIの観測

Ato SessionのSource `407650fa4e5a359b2f6ed0371f09550e858cd979` の
[CLI run](https://github.com/ato-run/ato/actions/runs/37111939319) はUbuntu PASS、
macOS/Windows FAIL。
[architecture run](https://github.com/ato-run/ato/actions/runs/37111939576) はPASS。
このSession stackにはCI #1478が含まれない。失敗するbrowser/Runtimeの3ファイルと
portable test/CLI workflowは、比較済みbase `31a6d6b4` と同じblobである。
macOSではCLI lib 49件/MCP mode1件は通った後、旧portable process試験が同じworker
early-exitで失敗した。child logが残らず、元CIの具体的な原因は未確定。
Windowsでは同じUnix browser internalsのコンパイル失敗を観測した。
CI依存を含めたnative Windows/MSVC受入を成功とは報告しない。

API Source `57991090c7a84ae39883893e763ec61aba4f5ea8` の
[Activity CI](https://github.com/ato-run/ato-api/actions/runs/37110623145) はFAIL。
Activity/CORSの13失敗名・155 PASSはexact base `02a9e58b` / head `9b483d4a`
の比較と一致した。これは既存失敗の比較結果であり、修正済みとは扱わない。
[比較記録](evidence/formation-current-api-ci-comparison-20261003.json)にlog hashと
失敗名を保存した。instance-state-syncはCI #729のfixture修正が含まれないため、
同じWebSocket handshake失敗。修正単独のnative Linux比較は
[CI記録](formation-agent-linux-ci-comparison-20261003.md)を参照する。

## 実受入の境界

実Native製品のSkill検出/明示起動、認証、tool inventory、provider通信、実OSSのfresh
same-K receipt、PWA入力/resume、今回探索artifactの機能/保存/新Run復元は未測定。
Producerへの値の完全非露出は、実製品/Runtime経路では未証明。固定fixtureの成功を
その代用にしない。独立有料API call、実受入Runtime attempt、remote migration、配備、
flag変更、通常Run許可、旧測定・失敗Search・UNKNOWN・予算台帳の変更は0。
