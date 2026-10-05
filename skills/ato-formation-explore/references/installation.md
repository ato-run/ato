# 配置と明示呼び出し

共通本文と補助ファイルは`skills/ato-formation-explore`に一度だけ置く。Installerは指定したProducer projectにsymlinkを作る。同じ場所への再配置は冪等で、既存の別Skillやリンクは上書きしない。設定済みの認証・エージェント設定・権限を変更しない。

```sh
python3 skills/ato-formation-explore/scripts/install.py \
  --project /absolute/path/to/producer-project --agent both
```

| 製品 | 配置先 | 明示呼び出し |
|---|---|---|
| Codex | `.agents/skills/ato-formation-explore` | `$ato-formation-explore` |
| Claude Code | `.claude/skills/ato-formation-explore` | `/ato-formation-explore` |

片方だけの配置には`--agent codex`または`--agent claude-code`を使う。symlink先のSkill treeだけをProducerへ公開し、元repoやOwner credential directoryへのアクセス権は付与しない。別hostへ移すときは共通Skill packageをそのhostへ配置してInstallerを実行する。

Codexは`agents/openai.yaml`の`allow_implicit_invocation: false`、Claude Codeは共通`SKILL.md`の`disable-model-invocation: true`で明示呼び出しを基本にする。これらは探索の認可・OS隔離を提供しない。shellを公開しない固定MCP入口の設定と実探索前の環境gateは[mcp.md](mcp.md)を読む。

配置方法は[Codex公式Skill資料](https://developers.openai.com/codex/skills)と[Claude Code公式Skill資料](https://code.claude.com/docs/en/skills)のlocal skill/symlink仕様に従う。製品versionだけで対応済みと判定せず、実受入でSkill検出・明示呼び出し・CLI操作を測定する。実測と未検証の一覧は受入記録に残す。

## OwnerからのNative Session起動

`scripts/native-session.py`は既存Searchへ接続するNative clientである。Sourceを起動せず、Dやreceiptを作らない。Ownerが既存`ato form --session-bridge`で開始した接続を指定すると、元の状態・agent binding・deadlineを確認し、公開Skill packageと限定Unix relayを用意して製品のSkillを明示呼び出しする。応答JSONの手渡しは不要である。対応を実測したmacOSのCodex 0.160.0／Claude Code 2.1.288に限定し、別versionやOSは開始を拒否する。

```sh
python3 skills/ato-formation-explore/scripts/native-session.py \
  --agent codex --version 0.160.0 \
  --connection "$OWNER_SESSION_CONNECTION" \
  --output "$WORKSPACE/.tmp/new-native-session" \
  --socket "$WORKSPACE/.tmp/formation-native.sock" \
  --binary "$CODEX_BINARY" --code-mode-host "$CODEX_CODE_MODE_HOST" \
  --mcp-binary "$FORMATION_MCP_BINARY" \
  --auth-file "$CODEX_AUTH_FILE" --model-catalog "$CODEX_MODEL_CATALOG" \
  --model "$SESSION_MODEL"
```

Claude Codeでは`--agent claude-code --version 2.1.288 --binary "$CLAUDE_BINARY"`とし、Codex専用の3引数は省略する。`--model`を指定する場合は開始時のSessionモデルに合わせる。出力先とsocketは新規、socketは100 byte以内の絶対pathとする。既存ファイルや別Searchの接続を上書きしない。

CodexはNative認証ホストとCode Modeを別プロセス・別OS profileにする。ホストは既存Native認証だけを読む。Code Modeには認証ファイル、Owner接続、Source filesystemを公開しない。model catalogからshell・file patch・追加toolを外し、固定MCP以外の実行入口を持たせない。

Claude CodeはOwner adapterが既存同一accountのKeychain credentialをRAMで読み、隔離したNative認証ホストへ渡す。ファイルやargvへ保存しない。[公式OAuth環境変数とsubprocess scrub](https://code.claude.com/docs/en/env-vars)を使い、MCP子プロセスにはcredentialを継承させない。公開するのは固定MCPの3 tool（status/next/submit）だけで、built-in tool・hook・plugin・追加MCPを無効にする。Native自身の一時出力もworkspace内に固定する。これらの設定はownerのAto credentialやRuntime ticketの共有を許可しない。

各turnは1つのexchangeを処理する。保存後はOwner側で状態を待ち、新しい未応答exchangeが成立したときだけ同じNative文脈で次のturnを始める。入力待ち・UNKNOWN・終端・期限で止まり、launch終了をSearch取消やRuntime cleanupの証拠にしない。`owner/launch-result.json`とNative eventsはOwner証跡であり、公開報告には保存済み結果を検査して必要な項目だけ移す。

Launcherの終了出力は、SDKから取得できたtoken/cached-token counterと費用推計を `SDK_reported_usage` へ自動でまとめる。Claudeの累計resultをturnごとに加算せず、切断した文脈はpartialとする。SDKのlist価格推計をaccountへの実請求と扱わない。token update数やSDK turn数を実LLM call数に代用しない。内部call数・実請求額は取得不能ならunknownのまま残す。未知のSDK fieldや非数値の内容は報告へコピーしない。

## 検証状況

2026-10-03のglobal製品inventoryは`codex-cli 0.46.0`、`Claude Code 2.1.288`。Installerによる両配置先からの同一Skill/補助ファイル参照、冪等再配置、既存Skill拒否はローカルで確認した。2026-10-04にはglobalのCodexを置き換えず、公式releaseの`Codex 0.160.0`と対応するCode Mode hostを受入専用に固定した。

実Codexの`skills/list`で共通packageを検出し、両製品のcontrolled provider fixtureで共通本文の明示展開と当時のFormationの4 toolを確認した。現在の契約はcancelをownerへ戻した3 toolであり、新pinのinventory受入を別途記録する。Codexでは`additional_tools`とCode Mode内部のtoolも確認する。これらはNative loader/tool構成の証拠であり、実provider推論、対象Sourceの独立探索、fresh receiptの受入結果ではない。新規Searchによる小規模受入はユーザーから実施指示を受けており、[環境gate](mcp.md)を満たしたセルから進める。過去の未実行計画の記録は変更しない。

ローカルCodex 0.46.0のhelp・同梱README・binaryにはSkill機能の証拠がなく、[同versionの公式Source](https://github.com/openai/codex/tree/rust-v0.46.0/codex-rs)にもSkill loaderがないため、このnative Skill経路には対応しない。本文を通常promptへ手動添付する方式を、native Skill受入の代用にしない。

repoの`.agents/skills`対応は[Codex 0.94.0](https://github.com/openai/codex/releases/tag/rust-v0.94.0)、`policy.allow_implicit_invocation`対応は[Codex 0.99.0](https://github.com/openai/codex/releases/tag/rust-v0.99.0)で追加されている。[0.99.0の公式loader](https://github.com/openai/codex/blob/rust-v0.99.0/codex-rs/core/src/skills/loader.rs)ではrepo Skill directoryのsymlinkを辿り、共通frontmatterのClaude固有fieldを拒否しない。今回要求の配置とpolicyに必要な機能下限は0.99.0と判断するが、このversionで本Skillを運用した受入結果はない。

実探索の前に、選定したCodex versionがこの下限以上であることと、両製品で同一packageの検出・明示呼び出し・policyの効力を確認し、versionを新しい測定IDへ固定する。確認できない場合は該当セルを開始せず、Ownerへ引き継ぐ。認証確認、製品更新、実inferenceも測定記録で区別する。

Skill creatorの`quick_validate.py`はClaude固有の`disable-model-invocation`を未対応fieldとして拒否する。共通packageではこの必須flagを保持し、Codex共通frontmatterの検査とClaude flagの検査を分ける。これはvalidation toolの制約であり、実loader互換を証明するものではない。
