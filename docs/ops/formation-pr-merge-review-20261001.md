# Formation PR統合レビュー — 2026-10-01

今回の範囲は現在成立している実装・測定・文書のmainへの統合です。
配備、remote migration、feature flag変更、追加のmodel/OSS実験は含みません。
マージcommitの`[skip ci]`でpush由来の自動配備・公開を抑止します。
workflow設定は変更しません。PRで実行されたCIの失敗は保持します。

## 依存順序とレビュー対象

| 対象 | レビューしたhead | 統合順序・範囲 |
| --- | --- | --- |
| Ato #1450 | `d56a0395400ea5a31fbc593cedad22c496b00c22` | baseline測定。merge `d5fab699113b797c9c2ea617d40e00257191378a` |
| Ato #1451 | `f3e715ab1b9a0568f86990fd674fe090a96896f1` | #1450後にmainへbase変更。merge `6b3133c36da8cbca84c118669b3fadd9c96c25f7` |
| API #712 | `fcbdf8881f3e61bfed7ca4375b9e937adad29d65` | Ato実装より先。merge `9e0031900836090612b94e775e26f980a68f32d7` |
| Ato #1452 | 運用code `0b9a9b9cea27fc9ee51e9e7be40b9d62c27be756`、テスト修正 `a1e5f8d8f51d16de6f8c7b94378af94fa15d1c15` | #1451後にmainへbase変更。現在の探索loopを統合 |
| Ato #1402 | 元head `2c1c6924286782f090783f1bd47a68e2e9b979ba` | 最新mainのroadmapを保持して文書入口だけを統合 |
| Ato #1421 | 元登録head `dc0fba4e0ecdd585985320551b52b72768002a5c` | 登録・fixture・harnessを統合。実験は実行しない |

## 測定資料

#1450の721件、#1451の583件、#1452 autonomous記録の323件のraw file hashを
tar archiveの実bytesと照合しました。各100件のrow数、7件のtyped-K成功、
固定plan hash、登録時のharness hashとPython構文を確認しました。
baseline/adaptiveのruntime codeは変わらず、既存測定のbytesやUNKNOWN・予算履歴は変更しません。

## API検証とmigration

APIの比較baseは`2dceb36b81d2772fe9fc6c87fcfbecb861dc18b8`です。
`pnpm typecheck`、`pnpm schema:check`、`pnpm schema:test-bootstrap`はPASS。
runtime-network、search-state、proposal migration、exploration upgrade、network budget、
variable bindingsの6 files / 210 testsは実local D1と同梱WASMでPASS。

CORS/Activitiesは同条件の最新mainとheadで同じ13 FAIL / 40 PASS、失敗名も一致しました。
`pnpm test:instance-state-sync`はWebKit保存・Chromiumのowner端末間同期を通過し、
base/head双方で既存invitee認可assertionの404≠403に停止しました。
CIのLinux WebKitとは別に実Chromiumでcoop-presenceを診断し、
双方でHTTP 503とhandshake拒否を再現しました。WebSocket経路とharnessに今回の差分はありません。
全CI成功、全browser acceptance成功とは報告しません。

Rust authority WASMはsource `5aa22a9eda19efaa41016d17d819c822bb135d2d`、ABI1、
SHA256 `c9d0246a943e801a6e818e083bcab778b69216ba3509de3a09d9f6776e6bb29c`を照合しました。
0308〜0314は最新mainの0307の後に追加され、実local D1移行で既存row保持を確認しています。
staging/productionのremote migration一覧を読み取り確認し、0308〜0314は双方未適用でした。
stagingには0288以降、productionには0298以降など、それ以前の未適用分もあります。
0194も一覧に残っているため、将来の配備では別途drift確認と順序計画が必要です。
今回remote適用・番号変更は行いません。

## Rust最終検証

all-targets検証で見つかったテストの`reasoning: None`初期化漏れと不要cloneの
Clippy警告を修正しました。運用codeは変更していません。
最終検証はこのworktree専用の新規Cargo targetで行います。

Formation 118、探索requirements 6、探索search 26、Worker 57、receipt authority 7、
Runtime 68、netd 71、合計353 testsがPASS。
macOSではLinux専用exploration containment testsは0件で、実Linux受入の代用にはしません。
netdの2件はpackage cwd直下の`.tmp`をsocketに使うため、長いworktree pathで
Unix socket長制限に失敗しました。TMPDIR変更では解消しないため、同じ新規targetの
test binaryをworkspace内の短い専用`.tmp/fmr-20261001`から実行して71 tests PASSを確認しました。
全対象all-targets Clippy `-D warnings`とfmtはPASS。

macOSのCI失敗対象`a_process_run_is_owned_by_its_run_until_stopped`は
headと比較mainの双方でローカルPASSでした。CI環境の原因解明とは区別します。
Windowsはexact base `e494e937`のCIにも12件のUnix API compile errorsがあり、
今回のWindows CIも同分類です。Windows実機の成立は主張しません。
PRの既存CIはgreenではなく、テスト修正headの新CIもローカル検証と別に扱います。
テスト修正headではUbuntu CLI・architecture・snapshotなどはPASS、Windows CLIはFAIL、
workspaceとmacOSの一部はこの記録時点で実行中です。全体greenとは扱いません。

ローカルログとdigestは[evidence manifest](evidence/formation-merge-review-20261001/manifest.json)に保存しています。

## 継続する制約

native OSS実測後のembedding guard・HTTP期限は最終コードで回帰検証しましたが、
新たなnative OSS/model再測定は行っていません。Python sdist、npm native build、
OCI追加接続、Runtime custom retry伝播、専用入力UI/CLI、実外部credential、
native失効、不確定Runtime切断などは後続作業です。
成功した探索提出は`k_reached_awaiting_assessment`のままで、Run・公開・配備の許可に変換しません。

E2登録のcode/artifact/API pinは変更せず、現mainのbytesとの差はfail closedで拒否します。
元の登録に沿う実験には元のpinの独立checkoutが必要です。現在の実装を測る場合は
実行前に別のprospective registrationを作ります。今回oracle/primary/model callは実行しません。
文書入口の統合はaccepted/draftの区別を保持し、最新roadmapや現在の機能状態を古い表へ戻しません。
