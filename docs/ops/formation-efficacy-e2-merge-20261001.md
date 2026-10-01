# E2登録と現行mainの統合境界

2026-10-01のマージ対象は、#1421の登録資料・fixture・評価harnessです。
oracle、72 primary cells、48 model callsの実行は今回の範囲に含みません。
過去の「review only / do not merge」は今回のマージ依頼で更新されましたが、
実験実行の保留は継続します。マージによって5bの実験結果を追加しません。

元の登録headは`dc0fba4e0ecdd585985320551b52b72768002a5c`、
評価artifact baseは`34c2ef2a8882f6fc53438c1e0f1722b240ed3d6e`、
API pinは`f7d866cbef7768f67b46840766497fbf806640fe`です。
[登録plan](formation-efficacy-e2-plan.json)のbytes、code/artifact/API pin、
予算、24 oracle classesは変更しません。

現行mainには登録以降のFormation実装が含まれ、
`apps/formation-worker/examples/formation_search.rs`などのbytesが元の
登録hashと異なります。mainでそのまま実験を開始しても、
`verify_registration`がモデルkey取得・cell予約前にcode driftとして拒否します。
この拒否を解除するためにhashやAPI pinを現在値へ読み替えてはいけません。
元の登録に沿う実験には元のpinの独立checkoutとartifactが必要です。
現在の実装を評価する場合は、実行前に別のprospective registrationを作成します。

統合時は最新mainのroadmapを保持します。過去の5b pending表や古いcoverage値を
復元せず、実験登録と、現在成立している探索loopの受入を区別します。
PythonのE1/E2 fixture・analysis・protocol・mock orchestration 91 testsは通過しました。
Rustの`generation_context_v2` 12 testsと`formation_search` example 5 tests、
対象のclippy（`-D warnings`）とworkspace全体のfmt checkも通過しました。
これは実oracle・Runtime cell・model callの実行結果ではありません。
