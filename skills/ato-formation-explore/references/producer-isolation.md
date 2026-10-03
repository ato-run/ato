# Producer隔離の準備とmacOS preflight

`scripts/producer-isolation-preflight.py`は、公開Skillの独立copy、固定binaryのdigest、Producer専用Seatbelt profileを準備し、OSの負例fixtureだけを実行する。Native agent、認証、Search、Runtimeを起動する操作は持たない。実探索前の[環境gate](mcp.md)を代替しない。

```sh
python3 scripts/producer-isolation-preflight.py prepare \
  --output /absolute/path/to/workspace/.tmp/fresh-producer-package \
  --measurement-id producer-isolation-new-measurement \
  --execution-pin <40桁の実装SHA> \
  --mcp-binary /absolute/path/to/ato-formation-session-mcp \
  --codex-binary /absolute/path/to/codex --codex-version <実測したexact版> \
  --claude-binary /absolute/path/to/claude --claude-version <実測したexact版>
python3 scripts/producer-isolation-preflight.py verify \
  --package /absolute/path/to/workspace/.tmp/fresh-producer-package
python3 scripts/producer-isolation-preflight.py probe \
  --package /absolute/path/to/workspace/.tmp/fresh-producer-package
```

上のpathは共通Skillをcurrent directoryにした場合。`--output`はこのSkillが配置されたproject/repo内の`.tmp`にある未作成directoryに限定する。既存directoryを上書き・再初期化せず、同じpackageでのprobe再実行も拒否する。versionは所有者が`--version`で確認した値であり、helperはNative CLIを呼ばない。

公開packageには`SKILL.md`、`agents/openai.yaml`、Markdown referencesと限定した補助scriptだけをcopyする。repo全体、tests、cache、auth/環境file、過去報告はcopyしない。コピー元のsymlinkや過大fileを拒否し、コピー中の変化、後から追加された公開file、Skill linkやMCP manifest/profile/binaryの変更も拒否する。両製品の配置先はcopy済みの同じSkillへ解決する。これはfilesystemでの確認であり、native Skill discoveryの確認ではない。

`public/fixed-mcp.json`は固定server名・binary digest・4toolの準備manifestである。製品のMCP設定fileや接続credentialではない。Session connectionをここへcopyしない。実MCP設定と限定接続は既存[MCP手順](mcp.md)に従い、承認済みのprivileged broker境界で結合する必要がある。

## profileと測定範囲

既存の`lib/sandbox/src/macos.rs`とCapsule用profileは変更しない。専用profileは`deny default`を基準に、公開treeの読取、専用scratchの読書き、system libraryの読取、選定したNative binaryと固定MCP binaryへのliteral `process-exec`だけを許可する。libSystemの起動に必要なroot directoryのliteral読取は既存sandboxと同じであり、root配下の包括読取は許可しない。shell/security/curl、任意binary、Sourceの直接起動、network/Unix socket、homeやKeychain fileへの包括許可は持たない。Sourceやowner directoryをmount/allow-listへ追加しない。

Native認証用に`com.apple.secd`と`com.apple.SecurityServer`のMach lookupを準備する。Keychainの実値を読み出すfixtureは実行せず、global login/設定も変更しない。Codexでは機能下限0.99.0以上と、計画専用のOS credential storeが必要。`cli_auth_credentials_store = "keyring"`はfile fallbackを避ける設定であり、既存global entryのコピーや新loginはhelperの仕事ではない。0600 credential fileを公開する代替もない。Claude Code 2.1.288のlocal helpとCodexの[対応下限](installation.md)は、実tool inventoryの測定と区別する。

`probe`は公開package内の固定C fixtureをclangでbuildし、共通のprofile generatorでそのfixture binary一つだけを実行可能にした専用profileを`sandbox-exec`へ渡す。Native用profileは生成済みだが実製品へ未適用であり、fixtureはNative binaryやMCPを起動しない。Source、owner credential、private grant、Runtime ticket、DB、過去報告、credential store fileの7クラスのprivate canaryを内部生成する。読取・書込、symlink escape、fork後の権限継承、Source直接実行、shell/security/curl実行、Docker相当Unix socket、任意TCP接続を確認する。存在する同一UIDのcanaryへの`EPERM/EACCES`を必要とし、file欠損やconnection refusedを拒否成功とは扱わない。

実値、captured stdout/stderr、例外、file bodyを出力しない。OSfixture結果はbool/count/digestで保存し、private canaryの値をdigestへ含めない。取得できないエージェント内部のcall数と費用は`unknown`。結果の`os_fixture_pass`は実Codex/Claude Code対応やsame-K PASSではなく、`native_acceptance_ready`は常にfalse。

## 実受入まで残るgate

同じNative process内のbuiltin/toolがMach認証IPCへ触れないことは、Seatbeltのfile/exec制限だけでは証明できない。実製品のtool inventoryとread/write/exec負例、設定・hook・plugin・subagentを通じた拡張の拒否を別途測定する。Codexのshell無効化とClaudeの選定済みrestricted/tool/strict MCP設定は、そのversionの実設定で確認する。このhelperは未確認の製品flagや任意agent起動commandを生成しない。

prepared profileはNative providerへのegressも、privateなSession connectionを読むMCP brokerもまだ許可していない。既存stdio MCPは接続fileを必要とするため、同じprofileへそのfileの読取を追加すればNative builtinからも見える可能性がある。fileを0600にして許可することで完成扱いにせず、固定MCPだけへ権限を渡すbroker境界を承認済み計画で確認する。それまではnative起動・ログイン・探索を開始しない。

OS fixtureの成功後も、承認済みplan、対応version、native Skill発見、固定MCP inventory、認証とtoolの分離、provider egressの各gateが未完了なら人へ引き継ぐ。過去測定の非露出保証や成功Dは変更しない。
