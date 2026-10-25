# Native controls / Source / Model / Group integration

2026-10-24。統合source `335557cfc79129f5331cf0fa87c0f139dcfffd65`、branch
`integration/formation-native-models-v1`。既存Source #1538/Models #1535/Group #1537へ、
Native controls #1539/#1540/#1541/#1542の共通base a322c898を競合なしで統合した。
各PRのfrozen baseは変更していない。

macOSのnetd/IPC/Connected Worker、全targetを直列実行し、448件成功・失敗0・既存18件
ignored、14 executable。3 crate全target Clippy `-D warnings`、workspace fmt、diff check成功。
CLI/portable全体やLinux/shared Runnerの再検証ではない。

最初の実行は3件の既存netd egressがcontrol socket待ちで失敗した。深い専用TMPDIRから
計138 byteのUnix socket pathを作り、macOS sun_path上限104 byteを超えていた。
同じsource/binaryと制限で、workspace直下の短いfresh rootへ変更した。推定88 byteとなり、
該当3件を含む全targetが成功した。元の失敗ログも保持し、テストのskip/timeout変更はなし。
ATO_HOME/TMPDIR/exploration rootだけを明示し、HOME/CODEX_HOMEは再定義していない。

新しい実GUI/Chrome共同fixtureは別のfrozen base/PRで検証中。この448件を実Chrome・
Linux GUI・Hosted認証済みRun・運営key・実GPUの成功へ読み替えない。shared Linux Cargo
はfixture authorの専有のまま、rootから使用していない。

コマンド・各executable結果・初回/短root両ログ・Clippy/fmtは作業用
`.tmp/native-controls-integration/`へ保存。配信側は両所有repoのstage16 opsを参照。
本検査でRunner install、API/PWA deploy、feature/key/migration/job/Run/GPU/本番変更はない。
