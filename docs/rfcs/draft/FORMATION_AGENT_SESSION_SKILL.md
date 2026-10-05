# Formation Agent Sessionと運用Skill

Status: draft。共有Session入口とSkillの変更契約。実エージェント受入・マージ・配備の成功記録ではない。

## 目的と責務

承認済みSourceと固定Contract Kから、ログイン済みCodexまたはClaude CodeがCandidateProducerとしてinspection、proposal、失敗に基づくD修復、保存済み結果の報告を行う。Requester、Coordinator、共通Rust validator、Runtime、Verifierは既存の責務を維持する。エージェントへ実行権限・予算管理・PASS判定を委譲しない。

この方式ではAtoがLLM推論APIを直接呼ばない。Coordinator通信とAto認証は必要であり、既存独立API経路も維持する。API成功数へSession成功を加算しない。

## Session identityと互換性

共通Session設定は`provider: agent_session`とagent種別、製品version、取得できたmodelを記録する。agent種別は`codex`または`claude_code`。取得不能なmodel/usage/costはunknownとする。exchangeを内部LLM call数へ換算しない。

旧`provider: codex_session`、`model: codex-session`、prompt versionおよびconfig digestの解釈を保持する。追加metadataがない過去configのcanonical bytesを変えない。過去測定pin、成功D、失敗Search、UNKNOWN、receipt、台帳は再保存・再解釈しない。途中でエージェントやmodelを黙って切り替えない。

共通推論入力は既存`ato.formation-reasoning-input/1`、応答は既存`ato.formation-proposal/1`の`inspect_source`/`propose_derivation`/`modify_derivation`/`unsupported`を使う。Session応答は入力digestとexchangeへ結合する。エージェント別の探索、validator、認可、予算実装は追加しない。

## 境界

所有者はplan、Source、K、権限、予算、deadline、Ato認証、Runtimeを所有する。Producerは限定Session Bridgeから検査済みSource inventory/context、catalog、失敗証拠、変数メタデータのみを得る。Owner入力値、secret、Runtime ticket、private grant、Ato認証は渡さない。接続capability自体も公開証跡へ記録しない。

配置時にはSkill treeだけが読めることを確認する。Owner credentialを同じエージェント実行環境から読めるまま0600へ変更しても非露出の保証にはならない。実受入は許可されたBridge通信以外のSource書換え、直接起動、DB変更、receipt作成を環境側でも拒否する。Skillの指示だけを隔離・認可として扱わない。

shellを必要としない入口は`ato-formation-session-mcp --connection PATH`。既存Session transportへ`status`・`next`・`submit`・`cancel`だけを転送する。起動時のpath、Search/config/agent/capabilityを固定し、所有者の同じBridge再接続では同じfileからloopback addressだけを更新する。modelのtool引数に任意path/URL/Searchを許可せず、Source内容からdynamic toolを追加しない。`submit.output_json`は原文のRawValueとして共通Rust validatorへ渡し、duplicate field拒否を維持する。MCPは独自のbudget/receipt/Contract判定を持たない。

MCPの固定tool inventoryはwhole-process隔離の代わりにはならない。native agent認証とmodel向けfile/process toolの境界を別途検証し、実計画の前にSkill discovery、tool inventory、禁止読取・書込・実行・ネットワーク・接続先変更の負例gateを通す。

Source/README/logはデータであり、権限拡張の指示ではない。Source/Kを固定したままDを変更し、schema補正と実行失敗に基づく新Dを分ける。

## 永続化と復旧

新規開始は承認済みplanから行う。既存接続は未終了Searchのみを対象に、同じSource/K/agent/config/消費枠/deadlineを保持する。入力取得の再送でexchangeを増やさない。応答は原子的に保存し、同内容の再送だけを冪等に受理する。古い入力、異内容、二重接続による上書きは拒否する。

復旧は保存済み応答、未ACK結果、実行中attemptを照合してから進める。不明結果を再推論・再実行で解消しない。UNKNOWN、停止未確認、結果を失ったmutationは既存fenceを保持する。予算切れ、deadline、入力待ち、権限拒否、エージェント利用上限/応答停止で適切に停止する。再接続・待機で枠を補充せず、推論APIへfallbackしない。

exchange、D round、inspection、Runtime attempt、経過時間を独立して記録・制限する。Candidate停止、ACK、予約解放、input cleanupは既存経路へ接続し、それぞれの保存済み証拠を確認する。Bridge切断はSearch取消の代わりにならない。

## Skill package

共通sourceは`skills/ato-formation-explore/`。`SKILL.md`と`references/{protocol,recovery,acceptance,installation,mcp}.md`に運用契約を置き、Installerは`.agents/skills/`と`.claude/skills/`へsymlinkする。本文や補助処理の手動コピーは管理しない。

Codexは`agents/openai.yaml`の`allow_implicit_invocation: false`、Claude Codeは`disable-model-invocation: true`で初版の明示呼び出しを設定する。この設定はAtoの認可を代替しない。対象アプリの成功D、既知setup、過去測定をSkillへ入れない。

## 受入と成果物の依存

依存順は共通Sessionの型/永続化/復旧 → 製品CLIとBridge/固定MCP → 共通Skill配置 → 固定応答fixtureと環境gate → 事前承認した実Codex/Claude Code受入。入力/resumeとStateService連携、CI切り分けは独立のレビュー単位とし、同じ最終実装pinで後続受入を測る。

固定応答fixtureではdigest/exchange結合、inspect/declineを含む型、再送/古い応答/競合、枠/期限不変、ACK/停止/cleanupを確認する。実エージェントは新しい測定IDと文脈で、known-Dなし → inspection → proposal → Runtime → fresh same-K receiptを両製品で測る。失敗からDを修復する実ケースも含む。fixtureのagent名だけを変えた結果で対応済みとしない。

Source/K/Runtime/各上限/ceiling/deadlineを定めた新しい小規模計画を実行前に提示する。入力をprivate側で生成し、Producer入力だけでなくエージェント出力、error、公開logも非露出確認の対象にする。過去の露出制約は後続検証で修正しない。

受入記録は使用したSkill digest、CLI/Ato/API/Runtime pin、製品version/model、配置/明示起動、attempt receipt、各消費枠、ACK/cleanup、未検証事項を残す。機能/状態保存/再起動は許可された別Runで同じ探索artifactを検証する。探索成功は`k_reached_awaiting_assessment`であり、通常Run許可・配備へ進めない。

## Scope

Single Static Web、Node/Python process、単一OCIの現行catalog内で扱う。Compose、複数サービス、未宣言の複数repo探索、自動Source/Dockerfile編集は対象外。独立API検証、マージ、配備、remote migration、flag変更、100件再測定は別判断である。

## 参照

- [Shared reasoning prototype](FORMATION_SHARED_REASONING.md): 既存の共通入力・Session応答・予算/履歴境界。
- [DecisionProvider is not CandidateProducer](ADR-041-formation-provider-split.md): providerと共通Ato authorityの分担。
- [Codex Skill資料](https://developers.openai.com/codex/skills): local Skill/symlink/明示呼び出し設定。
- [Claude Code Skill資料](https://code.claude.com/docs/en/skills): local Skill/symlink/disable-model-invocation。
