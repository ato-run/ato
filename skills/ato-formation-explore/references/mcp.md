# 固定Session MCPと受入環境

所有者は実装pinでbuildした`ato-formation-session-mcp`をstdio MCP serverとして設定する。起動引数は既存Session Bridgeの限定接続ファイルだけ。

```sh
ato-formation-session-mcp --connection /absolute/path/to/session-connection.json
```

この起動はSearchを作らない。所有者の`ato form --session-bridge ...`が準備済みであることを確認する。MCP serverは起動時のファイルpath、Search、configuration、agent、capabilityを固定し、再接続時に同じファイルからloopback addressだけを更新する。model tool引数から別接続先へ切り替えられない。

| Tool | 引数 | 結果 |
|---|---|---|
| `status` | `{}` | 保存済み公開状態 |
| `next` | `{}` | 同じ公開viewと、回答可能な現在入力・固定instructions |
| `submit` | `exchange_id`, `input_sha256`, `output_json` | 共通Rust validatorが受理した応答の保存状態 |
| `cancel` | `{}` | 既存取消経路の公開状態。cleanup確認を代行しない |

`submit.output_json`は16 KiB以下のJSON文字列。このFormation facadeではMCP envelopeを含む入力frameを256 KiB以下とする。既存Activity MCPのmemo/Interaction入力へこの上限を適用しない。未知tool、追加のtool引数、過大frame、接続binding変更は拒否する。拒否出力には受信内容・ファイルpath・capability・parse error詳細を含めない。proposal schemaや予算判定をMCPに再実装せず、CLIと同じSessionへ提出する。

Codexの選定versionで次の設定fieldに対応していることを確認して、計画専用の設定へ登録する。[0.99.0の公式設定型](https://github.com/openai/codex/blob/rust-v0.99.0/codex-rs/core/src/config/types.rs)にもこのstdio設定とtool allowlistがある。global設定を更新しない。

```toml
[mcp_servers.formation]
command = "/absolute/path/to/ato-formation-session-mcp"
args = ["--connection", "/absolute/path/to/session-connection.json"]
enabled_tools = ["status", "next", "submit", "cancel"]
```

Claude Codeの計画専用MCP設定例:

```json
{"mcpServers":{"formation":{"command":"/absolute/path/to/ato-formation-session-mcp","args":["--connection","/absolute/path/to/session-connection.json"]}}}
```

設定形式は[Codex公式MCP資料](https://developers.openai.com/codex/mcp)と[Claude Code公式MCP資料](https://code.claude.com/docs/en/mcp)を参照する。この設定例とRust fixtureの成功は、native agentでの配置・Skill明示呼び出し・探索の実測を証明しない。

## 実探索前のgate

MCPだけではプロセス・filesystem・認証を隔離しない。実探索前に、選定versionとharnessで次を確認する。

1. エージェントのnative Skill discoveryと明示呼び出しが同一packageへ解決する。公開Skill本文とreferencesだけを読み取れる。対象SourceはBridgeの検査済み入力経由で渡し、Source workspaceをhostへmountしない。
2. modelへ公開するtool inventoryを記録する。shell・exec・patch・外部取得・任意MCP・subagent経由でhost実行や権限拡張ができないことを確認する。tool名の許可や「read-only」の表示だけで、credential非露出を判定しない。
3. whole-process環境にはAto owner directory、入力実値、private grant、Runtime ticket、DB、Docker socket、過去の探索報告を公開しない。canaryで禁止された読取・書込・process実行・接続先変更を拒否することを確認する。native agent自身の認証はmodel toolsから読めない別経路で管理する。
4. 同一応答の再送、古い応答、ACK不明、UNKNOWN、deadline、入力待ち、枠切れをfixtureで確認してから、承認済みの新測定を開始する。fixtureだけで実エージェント対応やsame-K PASSを報告しない。

[Claude Codeのsandbox](https://code.claude.com/docs/en/sandboxing)はBash以外のfile/MCP経路へ同じ隔離を保証しない。[CLIのrestricted/strict MCP/tool設定](https://code.claude.com/docs/en/cli-reference)も、選定した環境での負例検証と組み合わせる。Codexのshell無効化でも、modelが公開するfile/patch toolを別途確認する。

Claude Code 2.1.288では`--restricted`や`--setting-sources ""`がproject Skillの読み込みも止めることを実CLIのprovider fixtureで確認した。Skillを明示起動する構成では、公開packageだけのprojectを使い、`--setting-sources project --tools "" --strict-mcp-config`と固定Formation MCPを組み合わせる。公開projectにsettings/hooks/pluginsを持ち込まず、選定した`--settings`でhooksを無効にする。これらのflagはOS隔離の代用ではない。`--tools ""`でも`/ato-formation-explore`の明示展開と4つのMCP toolは成立する。

Codex 0.160.0ではmodelがCode Modeを要求する場合がある。`features.code_mode_host = false`だけでMCPを利用可能と判断しない。選定した公式Code Mode hostも固定し、`agents.enabled = false`、shell/exec・任意取得・画像読取・自動Skill依存導入などを制限した実tool inventoryを測定する。`additional_tools`内のtool定義とCode Modeに渡るnested toolも対象にする。通常の`tools` fieldだけを数えるとtoolを見落とす。host file/patchなどの残存能力はOS負例で拒否を確認し、それまで実探索を開始しない。

Codexの認証storeを用意する場合、[公式0.99.0 Source](https://github.com/openai/codex/blob/rust-v0.99.0/codex-rs/core/src/auth/storage.rs)では`cli_auth_credentials_store = "keyring"`がOS store必須、`auto`がfile fallbackを持つ。keyring entryはcanonical CODEX_HOMEから導かれるため、孤立した計画用CODEX_HOMEは既存のglobalログインを自動共有しない。必要な認証準備は計画承認後に所有者が行い、global entryを変更しない。keyring設定も任意host commandからの読出しを防ぐ証拠にはならない。

## Unix socket relay

UnixではOwner brokerを`--connection OWNER_CONNECTION --publish-relay NEW_DESCRIPTOR --relay-socket NEW_ABSOLUTE_SOCKET`で起動できる。Producer側の設定は同じ`--relay PRODUCER_DESCRIPTOR`を使う。新しい接続・descriptorはOwnerが作り、model tool引数からsocketやSearchを選び直さない。socket pathはcanonicalなparentを持つabsolute path、100 UTF-8 bytes以下とする。

`producer-isolation-preflight.py prepare --relay-socket ...`で、そのsocket一つだけを許可するprepared profileを作れる。`probe`の成功は物理的な接続範囲の証拠であり、Native推論・認証・fresh same-K receiptの証拠にはならない。TCPを広く許可してNative受入へ読み替えない。
