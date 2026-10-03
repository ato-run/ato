# Formation v0共通Session・Skillの引き継ぎ

2026-10-03。レビュー可能な実装をDraftの単位へ分けた。実Codex/Claude Codeの探索と
入力/状態の実機受入は未完了であり、本番提供完了として扱わない。
旧測定pin・失敗Search・UNKNOWN・receipt・予算台帳は変更していない。

## レビュー単位と依存

| 単位 | Draft PR | 内容 |
|---|---|---|
| 共通Session | [Ato #1479](https://github.com/ato-run/ato/pull/1479)、[API #730](https://github.com/ato-run/ato-api/pull/730) | 固定agent設定、限定CLI/MCP、外部relay、保存応答の照合/冪等、元deadline/枠、取消/ACK/cleanup。 |
| 共通Skill | [Ato #1480](https://github.com/ato-run/ato/pull/1480) | canonical本文、Codex/Claude配置、明示呼び出し、復旧/報告、Producer隔離の補助と未測定gate。 |
| 入力/状態 | [Ato #1481](https://github.com/ato-run/ato/pull/1481)、[API #731](https://github.com/ato-run/ato-api/pull/731)、[PWA #418](https://github.com/ato-run/ato-pwa/pull/418) | 共通StateService接続、writer fence/restore/commit、別承認の機能受入、private HTTP binding、同じ入力割当のresume fixture、PWA受入補助。 |
| CI | [Ato #1478](https://github.com/ato-run/ato/pull/1478)、[API #729](https://github.com/ato-run/ato-api/pull/729) | Windows portability、macOS起動診断/Python条件、Linux WebSocket fixture、exact base/head/Node比較。 |

着手前と引き継ぎ前にAto #1476 / API #727がOPEN Draft、headがそれぞれ
`31a6d6b4d6df03a7463058ea38f87b7ff1e525a3` /
`9b483d4a7df9aa2370636bd1b60c2fbcda98320b` であることを確認した。
新規PRはその依存差分を維持し、mainへ付け替えていない。

受入依存はCI #1478 → Session #1479 → Skill #1480。
API #730の取消契約もSession停止に必要。状態側はAto #1481のRust authority →
API #731（base #730）→ PWA入力経路。API #729はbrowser fixtureの検証依存である。
これらはレビュー/受入順であり、マージ・配備の許可を意味しない。

API mainは `9e0031900836090612b94e775e26f980a68f32d7`。
今回の追加migrationは0317（取消）、0318（Source/state登録）、0319（機能受入）。
remote適用は0。適用履歴は配備時の別preflightで確認する必要がある。

## 今回の検証水準

統合Rust Source `8bb3a2b1` は専用targetでWorker 100件、Runtime HTTP 7件、CLI 49件、
Portable Application 15件、MCP mode/Activity各1件、strict clippy・format・buildがPASS。
[統合記録](formation-agent-integration-local-20261003.md)は過去のOSS測定と別ID。
元共有targetのコンパイル失敗も[別記録](formation-native-integration-cache-20261003.md)
に残し、専用targetの成功へ書き換えていない。

分離した状態Rust実装pinは `c8761b01`、authority WASMは2,511,227 bytes、SHA-256
`33ce99ba10a381944177ea93d89ad3596da05f750114ee7d73c89fffccc9a861`。
API関連fixtureは変更時点で214件PASS、後続のsame-assignment resume追加assertionは
同じ1ケースで個別PASS/typecheck。後続pinで全214件を再実行したとは報告しない。
PWA `7c96b53a` は関連6件/typecheck/build PASS、実CDP入力受入は0。

新しいGitHub CIはSession観測pin `407650fa` のUbuntu/architecture PASS、
macOS/Windows FAIL。CI依存 #1478を含まないheadであり、失敗対象のblobは比較済みbaseと
同じ。元macOS worker失敗の具体的な原因は未確定。API `57991090` のActivity/CORS13失敗名は
exact base/headと一致、WebSocket fixtureはCI #729が未包含。CI全面成功とは扱わない。

## 実探索前のgateと新規計画

新しい計画はSkill PRの `docs/ops/formation-agent-session-acceptance-plan-20261003.json`。
ID `formation-agent-session-20261003-a1`、statusは提案/未承認/未実行。
Codex・Claude CodeそれぞれでSVGOMG/Kutt、4 Searchを順次実施する案。
Source archive/Kは計画内で固定し、known Dは空。過去の成功D・報告はProducer公開packageに
含めない。Kuttの新規探索archiveと、今回の機能受入対象となる旧探索artifactを混同しない。

| 上限 | 各SVGOMG | 各Kutt | 4件合計 |
|---|---:|---:|---:|
| exchange | 6 | 8 | 28 |
| D round | 3 | 3 | 12 |
| inspection | 4 | 4 | 16 |
| Runtime attempt | 2 | 2 | 8 |
| deadline | 30分 | 30分 | 順次最大120分 |

Runtime案はfreshな隔離Linux Coordinator。実行pin、Node/npm/toolchainと利用可能Runtime、
容量を開始前に固定する。Atoの直接LLM API callは0の方式で、Native内部call/token/費用は
取得できなければunknown。既存の独立有料API枠とは別であり、残23回を補充/消費していない。

ローカルCodex 0.46.0は今回のnative Skillに未対応。互換製品version選定、同じSkillの
実検出/明示起動、認証、固定MCPだけのtool inventory、OS隔離とprovider通信の実測が必要。
Claude Code 2.1.288もversion確認にとどまる。0600、文書、agent名だけのfixtureを
private値非露出/実製品対応の証明にしない。未解決attempt/UNKNOWNを再実行しない。

機能/保存/再起動はAPI #731の
`docs/ops/formation-v0/functional-acceptance-proposal-20261003.{json,md}` にある別提案。
Kuttの今回探索artifactそのもの、changedetection.ioのcanonical retained provenance、
owned Runtime/Instance、容量、別のRun許可が必要。欠けたartifactを旧機能fixtureで代用せず、
Source/K/Dを書き換えたり手動mountしたりしない。PWA入力/許可変更後の実resumeも未測定。

独立API検証は有効credentialが届いた後に残枠を再preflightする別経路。この実装をAPI成功に
加算しない。マージ・配備・remote migration・feature flag変更・通常Run許可・100件再測定は
今回行っていない。
