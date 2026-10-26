# Windows修正を含む統合Sourceの追加検証

測定ID `formation-agent-integration-local-20261003-02`、clean Source
`b0408d46c9a4574139fd7ab1a7672f7e4c989c94`、macOS arm64/Rust1.96.0。
直前の統合 `8bb3a2b1` へWindows publication/lockとjournalテストを追加した。
旧測定01、旧共有targetの失敗、c876 WASM provenanceを変更していない。

専用 `$PWD/target`、当該`.tmp`のTMPDIR、`--locked --offline`、
`CARGO_INCREMENTAL=0`でCLI lib49、Worker lib100、journal7がPASS。
CLI/Worker all-target strict clippy、workspace format、MCP buildもPASS。
前pinのPortable Application15/Runtime HTTP7などをこの新pinの測定へ
付け替えず、変更したjournalと統合protocolの範囲を確認した。

新しい固定MCPは6,092,472 bytes、mode0555、SHA256
`3a56748bdc3ce8189b7f9bb81ec43f3e11f5aa38df493137ef7dc755e1aa9582`。
[機械記録](evidence/formation-agent-integration-windows-fix-local-20261003.json)に
各log hashと固定artifact pathを保存した。旧artifactを上書きしない。

これは統合fixture/buildで、実Codex/Claude Codeの探索やPWA/OSS機能受入ではない。
Producer通信・対応version・認証/tool inventoryと新規探索上限のgateは残る。
実Search/機能受入Run/有料API call/remote migration/配備/PRマージは0。
