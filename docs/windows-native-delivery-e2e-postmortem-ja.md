# Windows native delivery E2E 障害レポート

## 概要

Windows の native delivery E2E は、単一の不具合で落ち続けていたわけではなく、
Windows 固有の前提と fixture の不足が複数重なっていました。

今回の対応では、まず失敗の観測速度を上げるために Windows native delivery E2E だけを回せる fast path を CI に追加し、
そのうえで出てきた失敗を上から順に潰しました。

最終的に、dev 上の Build run 22940927711 で Native delivery E2E (Windows Fast) は success になっています。

## なぜ落ちまくっていたのか

根本原因は次の 4 系統です。

1. Windows 固有の実行条件が Linux/macOS より厳しく、stack size、PowerShell、Authenticode、ファイル属性の違いがそのまま効いていた。
2. native delivery の Windows fixture が、Tauri の実ビルドに必要な icon や bundle 周辺の前提を十分に満たしていなかった。
3. テスト側が CLI 出力形式や PowerShell 引数受け渡しにやや強く依存しており、Windows ではその前提が崩れやすかった。
4. 失敗の切り分けに毎回 Build 全体を待っていたため、1 個直すたびに次の失敗が見えるまで時間がかかっていた。

要するに、Windows native delivery の経路が壊れていたというより、Windows でだけ露出する前提違反が段階的に連鎖していました。

## 観測された主な失敗と試した対策

### 1. ローカルレジストリ起動時の stack overflow

- 症状:
  Windows job の早い段階でローカルレジストリ起動が stack overflow で落ちる。
- 対策:
  [.cargo/config.toml](.cargo/config.toml) で `x86_64-pc-windows-msvc` 向けに `/STACK:8388608` を追加した。
- 判断:
  これは有効だった。以後、失敗位置が先に進んだ。

### 2. Tauri fixture の icon / bundle 前提不足

- 症状:
  Windows の Tauri build が icon 不足または icon decode 失敗で落ちる。
- 対策:
  [tests/native_delivery_e2e.rs](tests/native_delivery_e2e.rs) で fixture 展開時に icon を補う helper を使うようにし、
  [tests/fixtures/native-delivery-tauri/src-tauri/tauri.conf.json](tests/fixtures/native-delivery-tauri/src-tauri/tauri.conf.json) では `bundle.active = false` と `icon = []` に寄せて、Windows bundle 前提を最小化した。
- 判断:
  fixture 側の不足を潰すのに有効だった。

### 3. build 出力 JSON の取り扱いが brittle

- 症状:
  Windows job が build 自体ではなく、JSON の parse や artifact path 解決で落ちることがあった。
- 対策:
  [tests/native_delivery_e2e.rs](tests/native_delivery_e2e.rs) で `artifact` を優先して解釈し、必要なら `dist/sample-native-capsule-0.1.1.capsule` や `find_capsule_artifact` に fall back するようにした。
- 判断:
  CLI 出力の揺れに対してテストを fail closed にしつつ、不要な brittle さを減らせた。

### 4. finalize 時の signing path が derived 出力に合っていない

- 症状:
  `Set-AuthenticodeSignature` が `src-tauri/target/release/sample-native-capsule.exe` を見に行って失敗する。
- 原因:
  finalize は fetched artifact を derived directory にコピーしてから、その derived directory を current directory にして署名処理を走らせる。
  したがって finalize 時には元の build path ではなく、コピーされた basename を使う必要がある。
- 対策:
  [tests/fixtures/native-delivery-tauri/ato.delivery.toml](tests/fixtures/native-delivery-tauri/ato.delivery.toml) の PowerShell finalize を `sample-native-capsule.exe` 向けに修正した。
- 判断:
  パス不一致の問題はこれで解消した。

### 5. finalize 後の exe が readonly で署名できない

- 症状:
  `Set-AuthenticodeSignature` が Access denied で落ちる。
- 原因:
  finalize の derived copy が元の readonly 属性を引き継いでおり、Windows 署名処理が書き込みできない。
- 対策:
  [src/native_delivery.rs](src/native_delivery.rs) で finalize 前に `ensure_tree_writable` を呼び、derived copy 配下の readonly 属性を再帰的に落とすようにした。
- 判断:
  これが最終的に必要だった本体側修正のひとつ。

### 6. 署名検証 helper が PowerShell の `$args[0]` に依存していた

- 症状:
  `Get-AuthenticodeSignature` 側で `FilePath` が null になり、検証フェーズで落ちる。
- 原因:
  `powershell.exe -Command` と追加引数の組み合わせに対する期待が、Windows runner 上では安定しなかった。
- 対策:
  [tests/native_delivery_e2e.rs](tests/native_delivery_e2e.rs) の `verify_authenticode_signature` を修正し、検証対象 path をコマンド文字列に埋め込む方式へ変更した。
- 判断:
  readonly 問題の後に残っていた最後の失敗原因だった。

### 7. 失敗の再現と確認に時間がかかりすぎた

- 症状:
  1 個修正しても、次の失敗が見えるまで Build 全体を待つ必要があり、調査サイクルが遅い。
- 対策:
  [.github/workflows/build-multi-os.yml](.github/workflows/build-multi-os.yml) に `native_only` fast path を追加し、
  push メッセージに `[native-only]` を含めるか `workflow_dispatch` で `native_only=true` を渡したときは Windows native delivery E2E だけを走らせるようにした。
  あわせて [.github/workflows/v3-parity.yml](.github/workflows/v3-parity.yml) もそのモードでは skip するようにした。
- 判断:
  直接の不具合修正ではないが、調査速度を大きく改善した。今回の解決で最も効いた運用面の変更はこれ。

## 最終的な結論

今回の Windows native delivery E2E 失敗は、単一の root cause ではなく次の組み合わせでした。

1. Windows 固有の実行条件を fixture と finalize 実装が十分に吸収できていなかった。
2. テスト helper が PowerShell と CLI 出力に対して brittle だった。
3. CI の観測経路が重く、連鎖的に現れる失敗を追いにくかった。

最終的に有効だった修正は次のとおりです。

1. Windows MSVC の stack size を増やした。
2. Windows Tauri fixture の icon / bundle 前提を整理した。
3. finalize 時の署名対象 path を derived 出力に合わせた。
4. finalize derived copy の readonly 属性を落として署名可能にした。
5. 署名検証 helper の `$args[0]` 依存をやめた。
6. native delivery E2E 専用の fast path を CI に追加した。

この結果、Windows native delivery E2E は dev 上で通る状態になった。

## Linux / macOS などターゲット追加時の注意点

### 1. fixture を「最小だが本物」にする

- Windows では icon と Authenticode、macOS では app bundle と codesign、Linux では ELF / desktop entry / chmod など、各 OS に固有の最低条件がある。
- テスト fixture は見た目だけのダミーではなく、その OS の build toolchain が本当に要求する最低構成を持たせる。
- bundle を使わない場合は、使わない前提を config で明示する。

### 2. finalize は「元 artifact」ではなく「derived artifact」を触る前提で設計する

- finalize の current directory、入力 path の rebasing、basename 化を先に決める。
- 外部署名ツールに渡す path は source build directory ではなく、finalize 時点の derived 出力に合わせる。
- 絶対 path と相対 path が混ざると OS ごとに壊れやすいので、contract を固定する。

### 3. 書き込み属性と実行属性を OS ごとに明示的に扱う

- Windows は readonly 属性で署名が壊れやすい。
- macOS と Linux は executable bit が足りないと fail closed にすべき。
- copy 時に metadata をそのまま引き継ぐだけで足りるとは考えない。

### 4. テスト helper は shell / PowerShell の引数規約に依存しすぎない

- `$args[0]`、シェル展開、引用符、パス区切り文字は OS 差分の温床になる。
- テスト helper は対象 path の quoting を自前で完結させる方が壊れにくい。
- JSON も 1 行 1 オブジェクトを前提にしすぎず、必要キーを軸に解釈する。

### 5. CI には「重い本番経路」と「切り分け用 fast path」を両方持つ

- 本番相当の matrix は必要だが、不具合調査のたびに全体を待つとボトルネックになる。
- 特定ターゲットの E2E を単体で流せる経路を最初から用意しておく。
- ただし PR の最終検証では fast path ではなく、通常の full check を必ず通す。

### 6. OS ごとに失敗メッセージを fail closed で明示する

- 署名失敗、必要ツール不足、パス不整合、permission 不整合は、曖昧な generic error にしない。
- 何が不足しているかをエラーメッセージで直接判断できるようにする。
- 今回も `Access denied` と `FilePath is null` が切り分けの決め手になった。

## 推奨する今後の運用

1. Windows native delivery を触る変更では、まず fast path で 1 回回す。
2. fast path が通ったら、通常の Build / V3 Parity / Secret Scan を含む full check で確認する。
3. 新しい OS ターゲットを増やすときは、fixture、署名/permission、外部ツール呼び出し、CI fast path の 4 点を最初に用意する。
4. native delivery の finalize contract をドキュメント化し、source path と derived path を混同しないようにする。
