# Formation Producer network profile: failed OS gates and fail-closed preflight

Nativeのloopback限定通信とpublic provider通信は未成立。実装pin
`237bac3637688730dd98188bbc5ef2727ccc367b`は、有効なrelay要求を
`blocked_scope_wider_than_loopback`、public provider要求を
`provider_egress_fixed_ip_unsupported`でbinary読取・package公開より前に拒否する。
default prepareだけがnetwork許可なしの準備fixtureを作る。
`native_acceptance_ready`はfalse。実Codex/Claude探索の受入完了ではない。

対象は独立worktreeの`feat/formation-producer-network-profile`、共通Skill baseは
`2b0541b888898c758d50ef6e00bf108a1a5cf22f`。元の測定01/02と既存Search、
receipt、台帳を変更していない。後続integrationへ測定pinを付け替えない。

| 測定ID末尾 | 実行pin | 結果 | 原因・範囲 |
|---|---|---|---|
| `20261003-03` | `b2277d82a011a0f42651b76bc944e0123fa08c28` | FAILED_BEFORE_OS_FIXTURE | 既存interfaceでsecondary loopback aliasをbindできずerrno49。Cをbuildしたがsandbox起動前にharness終了。 |
| `20261003-04` | `95d36192054129ad499ec14faab688eda59e2409` | FAILED_SBPL_COMPILATION | 数値IPのremote TCP構文をmacOSが拒否しrc65、C main未起動。bounded診断でhost文法がlocalhostを要求すると確認。 |
| `20261003-05` | `f3338d3966cee26384137e858ce2ca690acb8352` | FAILED_LOOPBACK_SCOPE | localhost構文でC起動、22項目中21成功。同host非loopback interfaceの同portへのlive接続を拒否できなかった。 |
| `20261003-06` | `237bac3637688730dd98188bbc5ef2727ccc367b` | PASS_FAIL_CLOSED_PREFLIGHT_ONLY | default prepare/verify、relay/provider要求の公開前拒否、既存output保全を確認。OS fixtureを再実行していない。 |

public package digest、profile/runner digest、保存した失敗boolと診断predicateは
[JSON evidence](evidence/formation-producer-network-profile-20261003.json)に記録した。
03では失敗記録を自動生成する前にharnessが終了した事実も残す。04/05の失敗を
新実装で成功へ読み替えない。

05ではIPv4・IPv6 loopbackの同port接続、live wrong portの拒否、Docker/Owner
相当Unix socketとUDPの拒否、8クラスの同UID private canaryの読取・書込拒否、
公開Skillの読取と書込拒否、scratch、symlink、fork継承、Source直接起動、
shell/security/curl拒否、extra descriptor非継承を確認した。file欠損・
connection refusedを拒否成功に数えていない。

失敗したnonloopback項目について、既存interfaceに同portのcontrolled listenerを
置き、sandbox外の到達性を確認した。別の限定C診断でもsandbox内のconnectが
実際に成功し、permission-deniedではなかったことをboolで保存した。
interface名・address・private値・raw stdout/stderrは公開していない。
global interface/aliasを変更せず、外部providerへ接続していない。

macOSの`remote tcp "localhost:PORT"`は測定した同host非loopback interfaceも
含む。対応する構文を使えたことだけでは必要なloopback限定境界を証明できない。
数値public IP構文も使えないため、両要求をfail closedにした。包括network許可、
wildcard、未測定proxyは追加していない。最新Native prepared profileには
network許可がない。C fixture用のlocalhost構文は実Native向けに使用可能とは扱わない。

検証コマンドは次のとおり。26件には、valid要求のbinary読取前拒否、未公開、
既存output保全、policy/hash改変拒否、private値を含まないerror出力を含む。

```sh
python3 -m unittest discover -s skills/ato-formation-explore/tests -p test_producer_isolation.py
/usr/bin/clang -Wall -Wextra -Werror -fsyntax-only skills/ato-formation-explore/scripts/producer-isolation-probe.c
git diff --check
```

すべてPASS。測定06では実helper CLIのdefault prepare/verifyがPASS。
relay/providerのfresh/既存packageを対象にした各拒否も期待するconstant codeと
空stdoutで確認し、11個の保存boolがtrue。追加OS診断は行っていない。

固定MCP manifestは`--relay <public>/session-relay.json`だけを記載し、
`--connection`、Owner file、capabilityをProducerへcopyしていない。実descriptorは
未発行・未読取、binding確認false。Ownerが独立発行したdescriptorのaddress一致、
保存済みSession deadline以下のexpiry、authは既存Rust authorityの別gateに残る。
親の固定binary metadataはSource `8bb3a2b1`、SHA256
`0aca096859d4f4410d3674c0ef061bd572762774b680995d423f2c8d619a62f8`。
この測定ではMCP実呼出しを行わず、別のintegration実測と混同しない。

Codex inventoryは0.46.0でSource feature floor 0.99.0未満。Claude Codeは
operator inventory 2.1.288。helperはnative CLIを呼ばず、対応versionの更新、
native Skill discovery、tool inventory、認証IPCとmodel toolの分離、loopback OS境界、
provider egressの確認は未完了。Native起動・auth/login・Search・推論dispatch・
Runtime attempt・有料API・配備・remote migration・flag変更はすべて0。
agent内部call数・費用はunknown。新planの実行承認を得た扱いにはしない。
