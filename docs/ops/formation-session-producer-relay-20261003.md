# Formation Session Producer relay: 2026-10-03

Ownerのprivate Session接続ファイルをNative sandbox外へ置くbrokerと、固定MCP入口を
Producer descriptorだけで利用するrelayを実装した。既存の4 toolsとSession authorityを
再利用し、新しい探索エンジン、予算台帳、PASS判定は追加していない。
本記録はsynthetic Session/MCP fixtureであり、実Codex/Claude Code受入ではない。

契約: [FORMATION_SESSION_PRODUCER_RELAY](../rfcs/draft/FORMATION_SESSION_PRODUCER_RELAY.md)。
集計: [証跡JSON](evidence/formation-session-producer-relay-20261003.json)。
既存のOSS測定、失敗Search、UNKNOWN、台帳、独立API成功数は変更していない。

## Source pinと初回失敗

独立worktree `ato-session-relay`、branch `feat/formation-session-producer-relay` は
Session reviewの `5f9df83592cbc735696e190b13635e76be8da9be` をbaseにした。
実装commitと後続証跡commitを分離した。

| 測定ID | 実行Source pin | 結果 |
|---|---|---|
| `formation-session-producer-relay-20261003-01` | `d0f3d5df1de94763132ca6d4794b0261d69fc219` | compile PASS、MCP lib 18件PASS、CLI grammar 1件FAIL |
| `formation-session-producer-relay-20261003-02` | `0936434435b7f3ed07741f7669690fd12b24c971` | MCP lib 20件＋CLI 1件PASS、native strict clippy/build/format PASS、startup 7 checks PASS |

初版ではClapの`requires`指定だけだと、mode groupにある`--relay`と
`--publish-relay`の不正な組み合わせがparseを通った。実処理のmatchは初版から
file読取前に拒否しており、Owner接続やSearchへの権限拡張は起きていない。
失敗を公開値のcase indexだけで再診断し、`conflicts_with = "relay"`を明示した。
初回測定は失敗のまま保持し、修正pinへ付け替えていない。

修正pinはpartial-byte受信でsocket timeoutが延長されない、whole-frameの絶対時間上限も
追加した。二重接続の同時提出は同じSessionの原子的保存へ集まり、同じ内容は冪等となる。

## 固定した入口

Owner側は既存のprivate接続を渡し、fresh descriptorを原子的にno-clobberで公開する。
以下は実装済みオプションの対応であり、本記録では実Searchに実行していない。

```text
ato-formation-session-mcp --connection OWNER_CONNECTION --publish-relay NEW_DESCRIPTOR
ato-formation-session-mcp --connection OWNER_CONNECTION --publish-relay NEW_DESCRIPTOR --relay-expiry-ms UNIX_MS
ato-formation-session-mcp --relay PRODUCER_DESCRIPTOR
ato-formation-session-mcp --connection OWNER_CONNECTION
```

最後は互換維持した旧stdio入口。publicationはOwner modeだけで許可され、expiryは保存済み
Search deadline以下。descriptorを読み直してbindingを変更する挙動や、再接続による
予算・deadline補充はない。停止したbrokerはSearchを取消せず、descriptorを上書きしない。
ACK/UNKNOWN/cleanupや期限後の保存応答はOwner authorityで照合する。

Producer descriptorはloopback address、独立したrandom 256-bit capability、Search ID、
configuration ref、agent metadata、expiryのみを含む。Ato auth、Owner capability/path、
ticket、grant、private値は含まない。Unixの0600は発行方法であり、同じUIDで動くmodelからの
非露出証明ではない。Ownerファイルのread拒否は別のwhole-process境界で必要となる。

capability自体はTCPへ送らず、requestとreplyを別domainのHMAC-SHA256で認証する。
nonce、固定binding、MCP frameへ結合し、旧portを取得した別processの偽応答とrequest proofの
reflectionを拒否する。MCP stdoutへtransport proof/capabilityやOwner pathを返さない。
`status`/`next`/`submit`/`cancel`以外のtoolやSource/path/URL/Search指定のtool引数は追加しない。
`output_json`内の重複fieldは文字列のまま共通Rust authorityへ渡し、共通の拒否を維持する。

## 実行した確認

macOS 15.8.1 build24H32、arm64、Rust 1.96.0。全Rustコマンドは`--locked --offline`、
`CARGO_INCREMENTAL=0`、共有target cache、各worktree内の`.tmp`をTMPDIRとして実行した。
dependencyの追加は既にlockにある`hmac 0.12.1`をCLI direct dependencyへ接続した2行だけ。

```text
cargo +1.96.0 check --locked --offline -p ato-cli --lib --bin ato-formation-session-mcp
cargo +1.96.0 test --locked --offline -p ato-cli --lib formation_session_mcp
cargo +1.96.0 test --locked --offline -p ato-cli --bin ato-formation-session-mcp
cargo +1.96.0 clippy --locked --offline -p ato-cli --lib --bin ato-formation-session-mcp -- -D warnings
cargo +1.96.0 build --locked --offline -p ato-cli --bin ato-formation-session-mcp
```

20件は既存MCP 7件＋新relay 13件。scope/auth/unknown-field拒否、fake broker、no-clobber、
expiry上限、frame上限とpartial drip、応答喪失後の状態照合、同時2接続、重複提出、古いdigest、
typed output重複field、取消後cleanup未確認、範囲外引数とエラー非露出を含む。
private canaryの値はfailure diffへ出さず、booleanを検証する。
本fixtureはRuntimeやCoordinatorへの実dispatchを行わない。

修正pinでbuildした固定MCP binaryは6,033,384 bytes、SHA-256
`d5359a89607e5228703835ab19ad9194ab33ef92bf6b10674c25af1424606c47`。
mutable shared targetから測定02の専用領域へコピーし、mode0555で保持した。
`--help`に実在する全オプション、生成したmalformed descriptorの拒否、競合argvの拒否、
stdout空、stderr固定、生成canary非露出の7 checksがPASS。raw capturesやcanary値は公開しない。
Native binary、認証、login、推論は呼び出していない。

## Windows依存と未測定

exact base/headを同じtoolchain/target/feature指定で比較した。

```text
cargo +1.96.0 clippy --locked --offline --target x86_64-pc-windows-gnu -p ato-cli --lib --bin ato-formation-session-mcp -- -D warnings
```

両方ともMCPに到達する前に、同じ`ato-runtime-attempt`依存の15 errorsでFAIL。
`browser_sandbox.rs`のUnix types/process_group/libc、`network_bridge.rs`のunused import、
`ephemeral.rs`のunused variable等。失敗する3 fileのGit blobはbase/headで完全一致
（digestはJSON参照）。これはCI review #1478の未適用依存であり、Session unitにはCI fileを
取り込んでいない。追加の広いcheckやfixはせず、最終integrationは#1478→Sessionの順とする。
native Windows/MSVCを成功済みとは扱わない。

実Native Skill discovery、model-facing tool inventory、Owner/private/Source/DB/socket read/write/
exec負例、OS credential-store認証IPCとmodel toolの分離、provider egress、同じOS profile内の
fixed broker接続は未測定。先のC-only OS fixture成功を、今回relayや実Native受入へ読み替えない。
最終integrationのsafe public projectionとpost-deadline reconciliation修正も別pinで確認する。

実Search/Runtime attempt/functional Run/有料API callは0。agent内部の実LLM call数・費用は
unknownとし、fixture/MCP exchangeから推計しない。実エージェントと機能受入の上限付き計画は
Owner承認待ち。配備、remote migration、flag変更、merge、global認証/設定変更は行っていない。
