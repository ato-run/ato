# CLI 0.9.4 / Capsule Hub — 公開と受け入れ（2026-11-17）

Status: Current — 公開インストーラーからD1・D2初回・D2再実行まで合格。

## 公開範囲

[CLI v0.9.4](https://github.com/ato-run/ato/releases/tag/v0.9.4) はstable releaseとして公開。
macOS arm64/x64、Linux arm64/x64、Windows x64の配布ビルドとHomebrew更新を既存release workflowで実施した。
実機受け入れはApple M1 / macOS 15.8.1。Windows/Linux native実行の成功を意味しない。
Web/APIの機能追加とUI修正はstagingのみ。本番Web/API配備・migration・feature flag変更は行っていない。
本番 `ato.run/install.sh` のbytesは変更していない。正式版受け入れ経路はstgの `install.sh --cli-only`。GitHub releaseの `ato-cli-installer.sh` も別の空HOMEへ導入し、同一binaryを確認した。

## 固定した組み合わせ

- CLI: `6b252a89f7d5abaed8b075fb6d6409f60c5ea3df` / 0.9.4 / release / clean。
- API: `b7c5caf512c521650bf57a0b5e8d51c883c3c0fe` / `63c3f0b9-142c-4496-b5c1-76bb9f46f434`。
- PWA: `176f1eeb1b54057e9e692532b2d7277a57e79813` / `b092255a-1615-47f9-8b55-a2ce77f2aeb5`。
- bundle: `sha256:35f940038bc66cec78eb53485738bf029d4ce68d619993a024f8a3a10c10c7e3`。
- Contract: `sha256:34d52388678111292fa890603110c23b47ce51220f452a28c233a2259e174c98`。
- D1: `sha256:f58cdca4697158f519a9a24d7bc36a7deb00a531b913fce3859deccbb46551e9`、D2: `sha256:382f1474eb77e0c18820f65a4d96edebfeddda6cc6b2b1cb5f4834bb9462406d`。
- staging migration: `0420_capsule_hub_derivation_info.sql`のみ適用。事前照合では未適用1件、旧journal-only名24件を記録し、履歴全体が一致したとは扱っていない。

共有lease下で検証し、前後のAPI/PWA active version一致とlease解放を確認した。
stgの公開installerを配備buildと比較し、既定版0.9.4を確認。GitHub installer・archiveはGitHubのSHA-256を照合し、両経路のインストール済みCLIとarchive内binaryの一致、version/commit/clean/releaseを確認。
HOME/ATO_HOME/XDG/CARGO_HOME/RUSTUP_HOME/TMPDIRを新規作成し、PATHは新規binとOS標準のみ。OSとApple Command Line Toolsは既存hostを使った。

## 実測

先行する検証は、両installerのexit 0後、補助cargo-dist検証のprefix指定を誤り停止した。D1/D2開始前で、CLIの実行失敗ではない。元ログを保存し、保存先指定を修正して別の空環境から以下を再実行した。

| Run | 所要時間 | attempt | fully_satisfied |
| --- | --- | --- | --- |
| d1 | 71.818秒 | `run-28120-1794915773072607000` | true |
| d2 | 1345.616秒 | `run-28290-1794915844302930000` | true |
| d2-second | 83.448秒 | `run-32208-1794917191765740000` | true |

すべて同一Contract・明示した別Derivation・異なるattemptのreceipt `/5`で検証した。
D2は承認済みの `--allow-native-source-build --allow-unsigned-native` を明示。
初回のsource treeとbuild profile、managed Rust 1.95.0の取得を確認した。
再実行はartifact・cache manifestのdigest一致と新attemptでbuild profile/source未作成を根拠にcache hitと判定。
cache keyは `sha256:b728ed78e125495d60c85b56e938cf9933a506bf5b2e02ae52002dcc3cce1212`。時間だけを根拠にしていない。全process groupを回収済み。
通常のCLI報告でrun_countは 10 → 13。verification/countの直接書き換えは行っていない。

## Hubの入力と表示

各Derivationに名前、初回/キャッシュ利用時の見積秒数、前提環境、依存関係（名前・版・取得方法・用途）、補足を保存できる。
CLI `ato hub describe`、API、所有者向けHub編集画面を同じversion/Derivation単位の情報へ接続した。
これは公開者の案内であり、immutable bundle/Contract/Derivation、実行時admissionやverificationを変更しない。
所有権、8KiB制限、未知field/重複/不正値、CAS revision競合をAPIで検証。
CLI/APIの実owner書込・読取、stale revision409、anonymous401、immutable情報不変をstagingで確認した。
公開stable binaryの匿名CLI読取でもD1/D2の保存済み情報との一致を確認した。
編集画面はDOMテストで検証。ログイン済み実ブラウザからの保存操作は未実施。

カプセル選択時の画面全体remountを除去し、一覧/検索/ページング/scrollを維持。
詳細fetch/retry/editを右パネルに分離し、選択行以外はmemo化。Settingsの重複route境界も整理し、別アカウント/編集対象の分離は維持。
一覧と詳細はviewport内で独立scrollし、狭い画面は一方のみ表示。「カプセルを公開」は「公開方法」の情報disclosureへ変更。
staging実ブラウザの1040×766 / 390×844で切替・内部scroll・一覧復帰を確認。73個の配信assetをローカルの配備buildと照合。

## 検証と記録

- CLI feature head34a7e6577およびstgリリースPR: Linux/macOS/WindowsのCLIテスト・Clippy、Rust workspace CI、architecture、docs、cargo-dist plan成功。重複push runは取り消し、成功したPR matrixと区別した。
- API: typecheckとHub85テスト。PWA metadata: typecheck/buildとHub52テスト。
- PWA rendering: typecheck/build、関連94テスト、追加Formation境界を含む14テスト。9locale・3,772message整合。
- stgのinstaller既定版を0.9.4へ更新（PWA #1037/#1038）。typecheck/build、既存installer5テストが成功。
- PWAのGitHub CIとHome SEO E2Eは記録時点でqueued。成功したローカル検証と区別する。
- staging統合候補と公開stable binaryの受け入れを別々に記録。旧0.9.4-stgのcold受け入れは履歴として維持。

[機械可読証跡](evidence/solidcraft-stable-094-acceptance-20261117.json) にrelease manifest、元receipt hashとpathを置換した投影、cache、配備、Hub API/UI検証を保存。
生ログ/元receiptはworkspace `.tmp/sc-094r-v06csta_`。UI棚卸はato-pwa `docs/ops/capsule-hub-panel-rendering-20261117.md`。
