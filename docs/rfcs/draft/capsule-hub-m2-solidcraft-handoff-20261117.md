# 引き継ぎ: Capsule Hub M2 — SolidCraft 2D の実機検証（2026-11-17 更新）

Status: Current（M1 実機 D1/D2・二回目 cache hit・staging 公開/Run 報告/PWA 表示まで完了。本番は未実施）
パスは特記がなければ ato-run workspace root 基準。14:20 時点の旧手順は以下の現状で置き換える。
親 Issue: ato-run/ato #1819（重複 Issue を作らない）
実装契約: `docs/rfcs/draft/capsule-hub-m2-native.md`
受け入れサマリ: `apps/ato/docs/ops/capsule-hub-m2-acceptance-20261116.md`

## 1. 目的（この作業が完了したと言える条件）

1. M1（macOS arm64）実機で `capsules/ato/solidcraft` の **2 つの D** が動くこと
   - D1 = Pre-built Native（上流 v0.43.0 の zip を digest 固定で参照、実行時に取得）
   - D2 = Source-built Native（commit `8ba46e9e2181396a9b5903b98031399aef61cf26` からローカルビルド）
2. 両 D の **Receipt が同一 `contract_ref`**、D ごとに異なる `derivation_ref`、`fully_satisfied: true`
3. ビルド成功と K 充足が分離されている（起動 → terminal exchange で K を観測）
4. 2 回目の D2 実行が **内容ハッシュ cache hit** になること
5. 受け入れ記録を更新し、本番公開の可否を別承認で判断する（**本番は未実施**）

## 2. 到達済み（dev / stg マージ済み）

### コード
| 領域 | PR | 内容 |
| --- | --- | --- |
| ato-api | #1287/#1288 | 公式 publisher 限定 1GB publish（migration 0419、presigned R2、complete 検証、sweep） |
| ato-api | #1289/#1290, #1291/#1292 | 大容量 publish の受け入れ記録と再現スクリプト |
| ato | #1824/#1825 | `ato.native@1` の remote digest 参照・Source-backed Native D・managed Rust toolchain・Adapter 監査 |
| ato | #1830/#1831 | remote native artifact の zip 展開 |
| ato | #1838/#1839 | v2 native terminal 観測（probe port 不要、`ato.contract.terminal@1`、`lib/pty-host`） |
| ato | #1840/#1841 | 宣言 argv（`ato.native.argv`、16/256B/printable、直接 exec） |
| ato | #1842/#1843 | 複数 terminal route が同一 Contract を実現 |
| ato | #1844/#1845 | SolidCraft 2D サンプル（`samples/solidcraft`） |
| ato | #1846/#1847 | staging 公開記録 |
| ato | #1848/#1849 | argv の descriptor 往復テスト |
| ato | #1850/#1851 | 実機検証スクリプト `scripts/acceptance/verify-solidcraft-native.sh` |
| ato | #1852/#1853 | M2 受け入れサマリ |
| ato | #1826/#1827 | multi-D 明示選択 UI・`--list-derivations` |
| ato | #1828/#1829 | 大容量 publish/download のストリーミング |
| ato | #1854/#1855 | ローカル実行者が native terminal surface を所有（admission 修正） |
| ato | #1856/#1857 | terminal route は URL を要求しない |
| ato | #1858/#1859 | ユーザーが受理した declared requirements を認可として運ぶ |
| ato | #1860/#1861 | run Adapter の egress 拒否は runtime phase 限定 |
| ato | #1862/#1863 | tar の pax メタデータヘッダを読み飛ばす |
| ato | #1865/#1866 | tar の symlink を root 内コピーとして配置 |
| ato | #1867/#1868 | source tree の展開上限を runtime artifact と分離（4GiB / 400k） |
| ato | #1869/#1870 | build sandbox に remote 443 + resolver port の grant を追加 |
| ato | #1836/#1837 | publish metadata の null 送信バグ修正 |
| ato-pwa | #1006/#1007 | Hub 詳細ペインの「実行方法」一覧 |
| ato-pwa | #1012/#1013 | staging デプロイ証跡 |

### 引き継ぎ前の staging 実測（履歴）
- 大容量 publish: `scripts/staging/capsule_hub_m2_large_publish_e2e.sh` が **11/11 pass**
- `capsules/ato/solidcraft` 公開済み: digest `sha256:1f1895ec…`、2 derivations、`native_terminal`、`macos/aarch64`、
  `verification=experimental / 0 runs`
- `https://stg-app.ato.run/capsules/ato/solidcraft` は 200 で「実行方法」2 行を表示
  （Worker `ato-pwa-staging` / version `7f4a720c-cad9-4098-a43d-1985348c2816`、source `origin/stg`）
- D1 は M1 実機で **成功**（`ok: pre-built Derivation ran`）

## 3. 引き継ぎ後に解消した問題

| 問題 | 対応 | dev / stg PR |
| --- | --- | --- |
| Cargo の DNS 失敗 | TCP/443 の既存 grant を維持し、mDNSResponder socket、resolver parent metadata、literal hosts/resolv.conf read を追加 | ato #1879 / #1881 |
| D2 が上流 Developer ID を要求 | ユーザーの明示承認により D2 のみ `require-valid-or-local-override`。実行時にも `--allow-unsigned-native` を明示 | ato #1882 / #1883 |
| 最終リンクで clang の一時ファイル作成失敗 | ビルド前に Run 専用 HOME/TMPDIR を作成。新たな host write grant は追加しない | ato #1884 / #1885 |
| 再実行の成功だけで cache hit と表示 | 二回目の実行成果物・cache manifest の digest 一致、source tree と build profile が未作成であることを検査 | ato #1882 / #1883 |

未コミットの `with_network(true)` 試行は採用しなかった。実測では socket 許可だけでも不足し、
`/etc`・`/var` の symlink metadata と resolver config の read が必要だった。
修正前 HTTPS は curl exit 6、修正後は exit 0、HTTP/80 は exit 7 で拒否。

その後の full build は依存取得・コンパイルを通過し、未作成 TMPDIR により最終リンクで失敗した。
同じ Seatbelt profile の C リンクはディレクトリ作成前に exit 1、作成後に exit 0。
元の長い Cargo stderr は CLI の先頭 2,000 文字制限に隠れたため、fingerprint diagnostics から回収した。

## 4. 現在のコードと実行

- runtime code: dev `bdd62dace9b6c1c295ad3a3baa3563d18e239139`、stg `dc1a1356cb5cef89d12f84a925cca1b4f4e8e328`。
- 作業 worktree: `apps/ato/.worktrees/m2-terminal`。
- ユーザー用 CLI: `apps/ato/.worktrees/m2-streaming/target/debug/ato`（上記 dev から再ビルド）。
- 修正後の再検証: `.tmp/solidcraft-verify-scratch-20261117-1530/`（m2-terminal 内）。
  D1・D2・D2 再実行は全件 `fully_satisfied=true`、二回目の cache hit も検証済み（16:01:28 JST 終了、exit 0）。
- managed toolchain / Cargo registry の carry-over は `ATO_TEST_REUSE_ENV_ROOT=1` と
  `ATO_TEST_ENV_ROOT` で明示。Run の source/target は新規作成している。
- 初回失敗のビルド成果物は cache に登録されていない。元の失敗した target は診断保存後に削除。
- 調査ログ: 同 worktree の `.tmp/resolver-fix/`。`acceptance-approved.log`、
  `approved-run-metadata.json`、`initial-linker-diagnostics.json`、`linker-scratch-probe.json` を参照。

bundle と refs:

| 項目 | 値 |
| --- | --- |
| bundle | `sha256:35f940038bc66cec78eb53485738bf029d4ce68d619993a024f8a3a10c10c7e3` |
| Contract | `sha256:34d52388678111292fa890603110c23b47ce51220f452a28c233a2259e174c98` |
| D1 | `sha256:f58cdca4697158f519a9a24d7bc36a7deb00a531b913fce3859deccbb46551e9` |
| D2 | `sha256:382f1474eb77e0c18820f65a4d96edebfeddda6cc6b2b1cb5f4834bb9462406d` |

署名方針の変更前と同じ K・D1 を確認済み。README は D の input tree に含まれるため、
今回の sample README は変更していない。Hub 表示用 README は bundle 外の metadata として更新できる。

## 5. 再現コマンド

```sh
cd ~/Ekoh/projects/ato-run/apps/ato/.worktrees/m2-terminal
ATO=/Users/egamikohsuke/Ekoh/projects/ato-run/apps/ato/.worktrees/m2-streaming/target/debug/ato \
  scripts/acceptance/verify-solidcraft-native.sh
```

既定では新しい `.tmp/solidcraft-verify-<UTC時刻>-<PID>/` と ATO_HOME を使う。
pack → list → D1 → D2（source build と local signature override を明示）→ D2 再実行 → receipt/cache 検査。
D1 には署名 override を渡さない。全成功時に三つの receipt と `cache-evidence.json` を保存する。

## 6. 受け入れ完了と次の境界

- 同一 bundle・K、別 D、三つの異なる attempt、全 observation の `satisfied` と
  `fully_satisfied: true` を確認。D2 の成果物は 191,193,936 bytes、artifact 上限は変更なし。
- `/5` receipt 自体には `build_cache` / `build_network` がないため、`cache-evidence.json` で
  receipt hash/attempt・artifact digest・cache manifest・再ビルドなしを別途確認した。
- 検証した同じ bundle bytes を共有 lease 下で staging の既存 `ato/solidcraft` に公開し、
  三つの実測 receipt を通常の Run 報告 API へ送信。**verified / run_count=3** を確認した。
  receipt の再送は `counted=false`、count は増えていない。
- API `fcb9b553-6768-4226-9580-9c5d2a0de5e9` と PWA `4229ab0f-823f-42d1-baa2-d852dc8461d0` は
  検証前後で同一。lease 手順は exit 0、解放済み。API/PWA 配備・migration・flag は変更していない。
- ブラウザーでも「確認済み・3 件の成功した実行」、新 D1/D2 の実行方法 2 行、D2 override コマンドを確認。
- 詳細と receipt hash は `apps/ato/docs/ops/capsule-hub-m2-acceptance-20261116.md` と
  `apps/ato/docs/ops/evidence/solidcraft-native-20261117.json`。Adapter audit も実測結果へ更新済み。
- **残る別判断は本番公開・本番デプロイ。今回の許可から推定しない。** 未実装事項は §7 と audit を参照。

## 7. 現在の制約

- 初回 full source build は大規模。toolchain の取得と source archive 217,936,976 bytes の取得も必要。
- cache lookup より先に source archive を取得するため、cache hit でも archive の再取得が発生する。
- build egress の hostname フィルタは未実装。実際の制限は TCP/443 と resolver 通信。
- `/5` receipt の build cache/network evidence 不足は上記の別証跡で扱う。
- CLI executable の Gatekeeper verdict は `rejected (the code is valid but does not seem to be an app)`。
  D1 の Developer ID / codesign 検査成功と区別し、notarization・stapling・revocation 検証済みとはしない。
- pixel surface、hosted native execution、Windows native containment などは Adapter audit の未実装一覧を維持。

## 8. 環境・検証の注意

- このセッションでは native Seatbelt を適用できる。以前のエージェント環境の拒否を現在の制約としない。
- worktree ごとに `CARGO_TARGET_DIR=$PWD/target` を使う。`apps/ato` 主 checkout の nightly は変更しない。
- Unix socket のテストは長い TMPDIR/cwd で失敗する。今回の短い test root は workspace `.tmp/m2t`。
- sandbox 27、native 76、追加負例を含む source-build 11、checker fixture 7、Clippy、arch-check は成功。
  full workspace 初回失敗分は短い path と逐次実行で全件再検証済み。元の全体コマンド成功とは表現しない。
- CI の formation 整形・README first-example・Linux PTY lint・Windows Unix API は base にもある失敗。
  課金エラーではなく、成功済みとも扱わない。
- staging origin は `https://staging.api.ato.run`。credential は origin に対応するものだけを使い、値をログに出さない。
- staging lease は `apps/ato-api/.tmp/hub-mvp/ato-api/scripts/staging-deploy-locked.mjs` で取得・解放する。
  他者の lease を削除しない。配備や feature flag の変更は今回不要。
- Node の Cloudflare 接続が特定の解決 IP で timeout した。今回だけの DNS preload を `.tmp/resolver-fix/`
  に置き、到達を確認した現在の DNS answer を使った。system DNS・exit node は変更していない。

## 9. 参照

- 実装契約: `docs/rfcs/draft/capsule-hub-m2-native.md`（§8 に以前のブロッカー記録、§9 に状態）
- Adapter 監査: `apps/ato/docs/rfcs/draft/native-desktop-adapter-audit.md`
- サンプル: `apps/ato/samples/solidcraft/`（`capsule.toml`, README, `box-measure.json`）
- 検証スクリプト: `apps/ato/scripts/acceptance/verify-solidcraft-native.sh`
- staging 受け入れ: `apps/ato-api/docs/ops/capsule-hub-m2-large-publish-20261116.md`
- staging PWA: `apps/ato-pwa/docs/ops/capsule-hub-m2-staging-deploy-20261117.md`
- 受け入れサマリ: `apps/ato/docs/ops/capsule-hub-m2-acceptance-20261116.md`
- 親 Issue: https://github.com/ato-run/ato/issues/1819
