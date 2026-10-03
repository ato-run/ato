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

Codexは`agents/openai.yaml`の`allow_implicit_invocation: false`、Claude Codeは共通`SKILL.md`の`disable-model-invocation: true`で明示呼び出しを基本にする。これらは探索の認可・OS隔離を提供しない。

配置方法は[Codex公式Skill資料](https://developers.openai.com/codex/skills)と[Claude Code公式Skill資料](https://code.claude.com/docs/en/skills)のlocal skill/symlink仕様に従う。製品versionだけで対応済みと判定せず、実受入でSkill検出・明示呼び出し・CLI操作を測定する。実測と未検証の一覧は受入記録に残す。

## 検証状況

2026-10-03のローカル製品inventoryは`codex-cli 0.46.0`、`Claude Code 2.1.288`。このversion確認はログイン・Skill検出・探索成功の証拠ではない。Installerによる両配置先からの同一Skill/補助ファイル参照、冪等再配置、既存Skill拒否はローカルで確認した。実製品での明示呼び出しと探索受入は別計画の実行前承認待ち。

ローカルCodex 0.46.0のhelp・同梱README・binaryにはSkill機能の証拠がなく、[同versionの公式Source](https://github.com/openai/codex/tree/rust-v0.46.0/codex-rs)にもSkill loaderがないため、このnative Skill経路には対応しない。本文を通常promptへ手動添付する方式を、native Skill受入の代用にしない。

repoの`.agents/skills`対応は[Codex 0.94.0](https://github.com/openai/codex/releases/tag/rust-v0.94.0)、`policy.allow_implicit_invocation`対応は[Codex 0.99.0](https://github.com/openai/codex/releases/tag/rust-v0.99.0)で追加されている。[0.99.0の公式loader](https://github.com/openai/codex/blob/rust-v0.99.0/codex-rs/core/src/skills/loader.rs)ではrepo Skill directoryのsymlinkを辿り、共通frontmatterのClaude固有fieldを拒否しない。今回要求の配置とpolicyに必要な機能下限は0.99.0と判断するが、このversionで本Skillを運用した受入結果はない。

実探索の前に、選定したCodex versionがこの下限以上であることと、両製品で同一packageの検出・明示呼び出し・policyの効力を確認し、versionを新しい測定IDへ固定する。確認できない場合は計画を開始せず、Ownerへ引き継ぐ。認証確認、製品更新、実inferenceはその計画に含めて別途実行する。

Skill creatorの`quick_validate.py`はClaude固有の`disable-model-invocation`を未対応fieldとして拒否する。共通packageではこの必須flagを保持し、Codex共通frontmatterの検査とClaude flagの検査を分ける。これはvalidation toolの制約であり、実loader互換を証明するものではない。
