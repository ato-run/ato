# Formation Producer隔離preflight

2026-10-03、公開Skill packageの準備とmacOS kernelの固定C fixtureを検証した。実Codex/Claude Code、認証、Search、Runtimeの受入は実行していない。過去の探索pin・成功D・失敗Search・台帳を変更していない。

| 測定 | 実装pin | 公開package SHA-256 | 結果 |
|---|---|---|---|
| `formation-producer-isolation-20261003-01` | `24fc63a71ba695bc2abafb0b0e24d58a288984c4` | `84d87786552847e97bc06779c2527e43435b28cbebe31bfa8da6614a20b00536` | libSystem初期化で停止。canary検査未到達。 |
| `formation-producer-isolation-20261003-02` | `29f327a44d5a1a3661ee19a270f5f14bbc11dfa3` | `e1cbe4689a226ae387ed55f8457f17a4d7510c8fe6b109c830256e8a2b2fd756` | 固定Cによる13項目PASS。native受入は未実施。 |

macOS 15.8.1 arm64、Apple clang 17.0.0で実行した。Sourceをmountせず、公開Skillを独立copyし、両製品のSkill配置先を同じcopyへ結び付けた。検証値は専用private側でrandom生成し、値・値のhash・raw stdout/stderrを公開していない。Source、owner credential、private grant、Runtime ticket、DB、過去報告、credential store fileは実物ではなく7クラスのcanary fileである。

## 失敗と修正

最初のprofileはroot directoryをmetadata読取だけに限定していた。固定Cはmain前にSIGABRTで停止し、stdout/stderrは0 bytes、自身のcrash記録は`ignition_halt`などのlibSystem初期化を指した。この起動失敗をprivate読取拒否やOS境界のPASSと扱っていない。

資格情報やエージェントを使わないprintf専用Cで切り分けた。全file読取を追加した条件は起動し、任意exec、Mach lookup/POSIX IPC、System/private/usr/dev/Libraryの単独許可、全metadata許可では起動しなかった。既存`lib/sandbox/src/macos.rs`と同じ`(allow file-read* (literal "/"))`だけを追加した条件は起動した。root配下の包括許可を追加せず、そのliteralを採用した。既存Capsule sandboxは変更していない。

失敗01のpackage/profileとbounded診断を残し、修正後は新しいdirectoryと測定ID02を使った。元pinを修正後へ付け替えていない。

## 検証結果

OSfixtureでは公開Skillの読取と専用scratchの書込が成功した。次の負例はすべて`EPERM/EACCES`で拒否され、同じUIDが読める存在済みcanaryの内容は変わっていない。

- 7クラスのprivate canaryへの読取・書込。
- 公開側のsymlink経由のprivate読取と、fork後のprivate読取。
- 公開Skillの書込、Source相当の直接実行。
- `/bin/sh`、`/usr/bin/security`、`/usr/bin/curl`の実行。
- 存在済みDocker相当Unix socketと、listen中の任意loopback TCPへの接続。

OSfixtureの実行許可は固定C runner一つに限定した。Native用profileは生成しただけで、実製品には適用していない。Mach認証IPCは準備したが、Keychain認証やcredential読取を実行していない。captured出力394 bytesはbounded boolean結果で、canary露出は検出しなかった。これは実エージェントの出力・error・公開logの非露出証明ではない。

package/authorityの15unit testもPASS。独立copy、配置先の一致、既存出力拒否、Source secret/cache/reportの除外、symlink/追加公開file/scratch redirectの拒否、package/profile/MCP tool拡張の拒否、version下限、非binaryの拒否、private値を含む不正manifestのerror非露出を確認した。Cの`-Wall -Wextra -Werror -fsyntax-only`と`git diff --check`もPASS。

## 残るgateと証跡

実装は[安定したpreflight手順](../../skills/ato-formation-explore/references/producer-isolation.md)にまとめた。`prepare`、`verify`、`probe`だけを持ち、Native起動command、login、global設定変更、product更新は行わない。

ローカルCodex 0.46.0はSource確認済みの機能下限0.99.0を満たさない。Claude Codeは2.1.288。helperへ指定したbinaryはdigestで固定したが、native Skill discovery、tool inventory/read/write/exec負例、認証IPCへのmodel toolからのアクセス拒否は未測定である。Keychain fileを0600で許可するfallbackはない。

privateなSession connectionをNative sandboxへ渡さずに固定MCPへ結合するexternal brokerと、選定したNative providerへのegressも未完了。prepared profileへcredential fileや任意networkを追加して完成扱いにしない。承認済みの新しいplan、対応version、native認証・tool境界・固定MCP・通信の各gateが揃うまで`native_acceptance_ready = false`を保持する。

[機械可読の証跡](evidence/formation-producer-isolation-preflight-20261003.json)に両測定pin/hash、診断、13チェック、選定binary hash、package内容、未測定gateを保存した。各raw packageとmetadataは実装worktreeの`.tmp/producer-isolation-20261003-01/`と`02/`へ保持した。実private credential読取、Native/auth/inference、Search、実Runtime attempt、有料API call、配備、remote migration、flag変更は0。エージェント内部の実LLM call数と費用は`unknown`。
