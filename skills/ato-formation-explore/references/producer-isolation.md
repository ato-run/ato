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

上のpathは共通Skillをcurrent directoryにした場合。この一時的なOS受入fixtureの`--output`は、Skillが配置されたproject/repo内の`.tmp`にある未作成directoryに限定する。通常のSkill配置は[installation.md](installation.md)、探索の接続は[protocol.md](protocol.md)の永続pathを使う。既存directoryを上書き・再初期化せず、同じpackageでのprobe再実行も拒否する。versionは所有者が`--version`で確認した値であり、helperはNative CLIを呼ばない。

公開packageには`SKILL.md`、`agents/openai.yaml`、Markdown referencesと限定した補助scriptだけをcopyする。repo全体、tests、cache、auth/環境file、過去報告はcopyしない。コピー元のsymlinkや過大fileを拒否し、コピー中の変化、後から追加された公開file、Skill linkやMCP manifest/profile/binaryの変更も拒否する。両製品の配置先はcopy済みの同じSkillへ解決する。これはfilesystemでの確認であり、native Skill discoveryの確認ではない。

`public/fixed-mcp.json`は固定server名・binary digest・4toolの準備manifestである。製品のMCP設定fileや接続credentialではない。Session connectionをここへcopyしない。実MCP設定と限定接続は既存[MCP手順](mcp.md)に従い、承認済みのprivileged broker境界で結合する必要がある。

固定MCP argvは`--relay <public>/session-relay.json`とし、`--connection`をProducerへ渡さない。prepare時にはdescriptorを作成せず、present/binding確認をfalseのまま記録する。Ownerが独立発行した実descriptorについて、既存Rust/auth経路で保存済みSessionの期限以下かを照合し、descriptorのaddressとOS profileのrelay endpointが完全一致することをpreflightで確認する。新しいfileを公開treeへ追加しただけでは既存packageのlayout照合を通らず、承認済みNative用の最終packageを別途固定する必要がある。この準備manifestはそのgateを完了にしない。

Ownerが通信先を固定した場合だけ、`prepare`へ`--relay-endpoint 127.0.0.1:PORT`と、最大4件の`--provider-endpoint NUMERIC_PUBLIC_IPV4:443`を明示できる。defaultは通信許可なし。endpointは数値IPv4と単一portに限定し、hostname、URL、wildcard、range、IPv6、providerへのprivate/loopback/link-local/multicast addressを拒否する。DNSや接続先の自動取得・変更・fallbackは行わない。このhelperはrelay descriptorやOwner接続fileを読まない。指定したTCP addressだけでSearchのscope/auth/expiryを証明せず、実descriptorの照合は既存Rust relay protocolへ残す。

## profileと測定範囲

既存の`lib/sandbox/src/macos.rs`とCapsule用profileは変更しない。専用profileは`deny default`を基準に、公開treeの読取、専用scratchの読書き、system libraryの読取、選定したNative binaryと固定MCP binaryへのliteral `process-exec`を許可する。libSystemの起動に必要なroot directoryのliteral読取は既存sandboxと同じであり、root配下の包括読取は許可しない。通信先を明示した場合も、exact `remote tcp "IP:PORT"`以外への許可、inbound、DNS、UDP、IPv6、Unix socket、system-socketの包括許可は追加しない。shell/security/curl、任意binary、Sourceの直接起動、homeやKeychain fileへの包括許可は持たない。Sourceやowner directoryをmount/allow-listへ追加しない。

Native認証用に`com.apple.secd`と`com.apple.SecurityServer`のMach lookupを準備する。Keychainの実値を読み出すfixtureは実行せず、global login/設定も変更しない。Codexでは機能下限0.99.0以上と、計画専用のOS credential storeが必要。`cli_auth_credentials_store = "keyring"`はfile fallbackを避ける設定であり、既存global entryのコピーや新loginはhelperの仕事ではない。0600 credential fileを公開する代替もない。Claude Code 2.1.288のlocal helpとCodexの[対応下限](installation.md)は、実tool inventoryの測定と区別する。

`probe`は公開package内の固定C fixtureをclangでbuildし、共通のprofile generatorでそのfixture binary一つだけを実行可能にした専用profileを`sandbox-exec`へ渡す。Native用profileは生成済みだが実製品へ未適用であり、fixtureはNative binaryやMCPを起動しない。Source、owner credential、owner connection、private grant、Runtime ticket、DB、過去報告、credential store fileの8クラスのprivate canaryを内部生成する。主loopbackの二つのTCP portでrelay/providerルールのpositive接続と、live wrong port、同じ許可portでlistenしたIPv6 loopback wrong address、UDP、Docker/owner相当Unix socketの拒否を同じOS profileで測る。public provider、実relay descriptor、Native認証には接続しない。extra fdを閉じ、C process内でも追加descriptorがないことを確認する。読取・書込、symlink escape、fork後の権限継承、Source直接実行、shell/security/curl実行も確認する。存在する同一UIDのcanaryとlive拒否先への`EPERM/EACCES`を必要とし、file欠損やconnection refusedを拒否成功とは扱わない。追加のsecondary IPv4 negativeも`EPERM/EACCES`だけを必要とするが、macOS既存interfaceではそのaddressをlistenできず、live到達性は未確認と明記する。global interface/aliasを変更しない。旧packageのprobeや旧測定の再解釈はせず、新しいpackage/測定IDを使う。

実値、captured stdout/stderr、例外、file bodyを出力しない。OSfixture結果はbool/count/digestで保存し、private canaryの値をdigestへ含めない。取得できないエージェント内部のcall数と費用は`unknown`。結果の`os_fixture_pass`は実Codex/Claude Code対応やsame-K PASSではなく、`native_acceptance_ready`は常にfalse。

## 実受入まで残るgate

同じNative process内のbuiltin/toolがMach認証IPCへ触れないことは、Seatbeltのfile/exec制限だけでは証明できない。実製品のtool inventoryとread/write/exec負例、設定・hook・plugin・subagentを通じた拡張の拒否を別途測定する。Codexのshell無効化とClaudeの選定済みrestricted/tool/strict MCP設定は、そのversionの実設定で確認する。このhelperは未確認の製品flagや任意agent起動commandを生成しない。

prepared profileには明示されたexact relay/provider TCP endpointだけを設定できる。[固定MCP broker](mcp.md)はOwner側でSession connectionを読み、Producer側は独立したrelay descriptorだけを使う。prepared profileへOwnerのfile読取を追加せず、選定したrelay endpointへの実MCP接続と実Native tool境界を承認済み計画で確認する。providerのIP:443許可はTLS/SNI identityやDNSを保証せず、共有CDNの同じIP上の別originも区別できない。Ownerがprovider identity・固定IP・TLS・実Nativeの接続方法を確認できない場合は停止する。未確認のDNS/Mach/network許可や製品flagを補って動作済みにしない。loopbackのprovider代用fixture成功をpublic provider到達やこのOS profileでの実製品認証・推論へ読み替えない。それまではnative起動・ログイン・探索を開始しない。

OS fixtureの成功後も、承認済みplan、対応version、native Skill発見、固定MCP inventory、認証とtoolの分離、provider egressの各gateが未完了なら人へ引き継ぐ。過去測定の非露出保証や成功Dは変更しない。
