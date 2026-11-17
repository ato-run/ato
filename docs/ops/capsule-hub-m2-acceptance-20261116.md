# Capsule Hub M2 — 受け入れサマリ（2026-11-16）

Status: M1 native D1/D2・二回目 cache hit・staging 公開/Run 報告/PWA 表示の受け入れ完了。本番は未実施。
Parent: ato #1819（重複 Issue なし）。契約: `docs/rfcs/draft/capsule-hub-m2-native.md`。

## 1. マージ済み PR（dev → stg 到達済み）

| 領域 | PR | 内容 |
| --- | --- | --- |
| API | ato-api #1287/#1288 | 公式 publisher 限定 1GB publish（migration 0419、presigned R2、complete 検証、sweep） |
| API | ato-api #1289/#1290 | 大容量 publish の staging 受け入れ記録 |
| API | ato-api #1291/#1292 | 再現可能な staging E2E スクリプト |
| CLI | ato #1836/#1837 | publish metadata の null 送信修正 |
| CLI | ato #1828/#1829 | 大容量 publish/download のストリーミング |
| Core | ato #1824/#1825 | remote digest 参照・Source-backed Native D・managed Rust toolchain・Adapter 監査 |
| Core | ato #1830/#1831 | remote native artifact の zip 展開 |
| Core | ato #1838/#1839 | v2 native terminal 観測（probe port 不要、`ato.contract.terminal@1`、`lib/pty-host`） |
| Core | ato #1840/#1841 | 宣言 argv（`ato.native.argv`） |
| Core | ato #1842/#1843 | 複数の terminal route が同一 Contract を実現 |
| Core | ato #1848/#1849 | argv の descriptor 往復テスト |
| CLI | ato #1826/#1827 | multi-D 明示選択 UI・`--list-derivations` |
| Samples | ato #1844/#1845 | SolidCraft 2D サンプル |
| Samples | ato #1846/#1847 | staging 公開記録 |
| Samples | ato #1850/#1851 | 2 D 実機検証スクリプト |
| Web | ato-pwa #1006/#1007 | Hub 詳細ペインの「実行方法」一覧 |
| Web | ato-pwa #990/#991 / #932 系 | M2 surfaces 表示（既存） |
| Web | ato-pwa #1012/#1013 | staging PWA 配信・「実行方法」2 行の実測記録 |
| Native | ato #1879/#1881 | HTTPS port grant を維持した macOS resolver socket/config 許可 |
| Samples | ato #1882/#1883 | D2 の明示的なローカル署名 override と厳密な receipt/cache 検査 |
| Native | ato #1884/#1885 | ビルド前の Run 専用 HOME/TMPDIR 作成 |

## 2. 実測済み（staging）

- 大容量 publish（`scripts/staging/capsule_hub_m2_large_publish_e2e.sh`）: **11/11 pass**
  — 公式 12.6MB の presigned PUT、未認証 resolve/artifact/download-url、一般 8MiB 超 413、
  特権偽装 403 `privilege_not_assertable`、非所有 namespace 403、1GB+1 413、
  未完了 complete 404 かつ version/tag なし、R2 checksum 不一致 403。
- 初回の `capsules/ato/solidcraft` 公開: `sha256:1f1895ec…`、2 derivations、`native_terminal`、
  `requires.platforms=["macos/aarch64"]`、`verification=experimental / 0 runs`（成功 Run のみ数える）。
- ページ `https://stg-app.ato.run/capsules/ato/solidcraft` は 200 で描画。

## 3. M1 再検証（2026-11-17）

このセッションでは Seatbelt を適用できた。以前の `sandbox_apply: Operation not permitted`
は今回の制約ではない。DNS 修正後の初回 Run は toolchain/crate の取得を通過し、最終リンクで
未作成の TMPDIR により失敗した。Run 専用の HOME/TMPDIR をビルド前に作る修正を追加した。

- 実行環境: Apple M1、macOS 15.8.1、arm64。
- Runtime code: dev `bdd62dace9b6c1c295ad3a3baa3563d18e239139`、stg `dc1a1356cb5cef89d12f84a925cca1b4f4e8e328`。
- ユーザー用 CLI: `apps/ato/.worktrees/m2-streaming/target/debug/ato`（上記 dev から再ビルド）。
- 実測 CLI は m2-terminal の working tree build（実装差分をそのまま `e97a128d3` に commit）。
  ユーザー用 CLI は同じ実装を含む clean な dev から再ビルドした。両 binary hash は証跡 JSON/ログに記録。
- D2 の `require-valid-or-local-override` と `--allow-unsigned-native` はユーザーが明示承認。
  D1 の宣言・署名要件は変更していない。K も変更していない。
- 再検証: `scripts/acceptance/verify-solidcraft-native.sh`。managed toolchain/Cargo registry の再利用を
  `ATO_TEST_REUSE_ENV_ROOT=1` と `ATO_TEST_ENV_ROOT` で明示し、Run の source/target は新規作成。
- bundle: `sha256:35f940038bc66cec78eb53485738bf029d4ce68d619993a024f8a3a10c10c7e3`。
- Contract: `sha256:34d52388678111292fa890603110c23b47ce51220f452a28c233a2259e174c98`。
- D1: `sha256:f58cdca4697158f519a9a24d7bc36a7deb00a531b913fce3859deccbb46551e9`。
- D2: `sha256:382f1474eb77e0c18820f65a4d96edebfeddda6cc6b2b1cb5f4834bb9462406d`。

**2026-11-17 15:28:38–16:01:28 JST の再検証は exit 0。** D1、D2、D2 再実行の
三つの `/5` receipt はすべて同じ bundle・K を参照し、選択した D と異なる attempt を記録する。
全 observation が `/initialize` / `satisfied` / `captured_bytes=3947` で、全件 `fully_satisfied=true`。
三つの実行 process group が回収済みであることも確認した。

| Run | attempt | signature | build |
| --- | --- | --- | --- |
| D1 | `run-80740-1794896934899701000` | Developer ID / require-valid / override false | upstream archive |
| D2 | `run-81307-1794897093237150000` | ad-hoc / require-valid-or-local-override / override true | 新規 source/target、build profile あり |
| D2 再実行 | `run-87170-1794898685073560000` | ad-hoc / require-valid-or-local-override / override true | cache hit、source/build profile なし |

D2 の成果物は **191,193,936 bytes**。artifact 上限 512MiB は変更せずに採用できた。
初回と再実行の binary digest は `sha256:52d921aae7e759e30bb7054ad14857695615b5fc6b157626ec4f9a90372ac6b6`、
cache key は `sha256:b728ed78e125495d60c85b56e938cf9933a506bf5b2e02ae52002dcc3cce1212`。
`cache-evidence.json` は二回目の receipt hash と attempt に結び付き、同じ staged artifact/cache manifest と
再ビルドなしを確認する。`build_cache` / `build_network` を `/5` receipt 内の field として扱わない。

両 D の codesign 検査は valid。Gatekeeper は standalone CLI に対して rejected と記録する。
notarization・stapling・revocation を実測したものではない。D2 の override は署名判断だけで、
filesystem・runtime egress・process group の制限は維持する。
証跡は `apps/ato/.worktrees/m2-terminal/.tmp/solidcraft-verify-scratch-20261117-1530/`、
調査ログは同 worktree の `.tmp/resolver-fix/` に保存する。

### 修正の検証

- 元の build profile で crates HTTPS は curl exit 6。resolver socket だけでは不足し、
  `/etc`・`/var` の metadata と resolver config read を加えると exit 0。HTTP/80 は exit 7 で拒否。
- 同じ build profile の C リンクは未作成 TMPDIR で exit 1、Run 内のディレクトリ作成後は exit 0。
  xcrun の host cache 拒否警告は残るが probe のリンクを妨げない。
- sandbox 27、native 76、追加負例を含む source-build 11、receipt/cache checker fixture 7 が成功。
  workspace Clippy、arch-check（50 packages）、CLI build、変更ファイルの整形・diff check が成功。
- 全 workspace test の初回は 3,185 pass / 38 fail / 29 ignored。失敗分は短い TMPDIR/cwd と
  逐次実行で全件再検証して成功（Unix socket path 上限、cache lock race、cold-start timeout）。
  元の全体コマンドを成功扱いにはしない。
- CI は既存の formation 整形、README の first-example、Linux PTY lint、Windows Unix API で失敗。
  base #1874 にもある問題と確認済み。今回の macOS Clippy は成功。CI 全体成功とは扱わない。

## 4. 検証済み bundle の staging 反映（2026-11-17）

共有 lease `solidcraft-m2-native-20261117` / release `solidcraft-m2-bdd62dace-35f94003` 下で、
上記の検証済み bundle bytes を `capsules/ato/solidcraft` の `latest` に公開した。
同じ digest に対する三つの実測 receipt を通常の Run 報告 API に送信し、各回 `counted=true`、
`run_count=1 → 2 → 3` を確認した。D1 receipt の再送は `counted=false`、count は 3 のまま。
最終状態は **verified / successful_runs=3 / run_count=3**、last_run_at は `2026-11-17 07:07:15` UTC。
count や verification を直接更新したものではない。

| 固定した対象 | 実測値 |
| --- | --- |
| API Worker | `ato-store-staging` / `fcb9b553-6768-4226-9580-9c5d2a0de5e9` |
| API の直前の code deployment | `65c65f8a22fba89435fb7ae8cde7038ce85227d7`（以降は Secret Change） |
| PWA Worker | `ato-pwa-staging` / `4229ab0f-823f-42d1-baa2-d852dc8461d0` |
| PWA source | `e1e3f33ecb9a156ddd929be35c827e50090896a6`（配信 service worker の marker と #1021 配備記録で確認） |
| PWA assets | `main-CVy1lYy1.js` / `main-BOtYRxC8.css` |

API と PWA の active version は lease 下の検証前後で一致し、health は 200。
Wrangler のテレメトリ待ちが発生したため、以降の管理コマンドだけ送信を無効にした。
ロック付き手順全体は exit 0、lease の解放も確認済み。API/PWA の再配備、migration、flag 変更は行っていない。

ブラウザー実機で [staging 詳細ページ](https://stg-app.ato.run/capsules/ato/solidcraft) の
「確認済み」「3 件の成功した実行」、新しい D1/D2 の実行方法 2 行、bundle digest、
D2 の明示 override コマンドを確認した。Hub README は metadata として更新し、bundle の input tree は変更していない。

[検証サマリ JSON](evidence/solidcraft-native-20261117.json) に receipt hash、各 observation、
signature 判断、cache evidence、配備の組み合わせとブラウザー結果を保存した。
元 receipt は §3 の work directory に残す。Run は CLI-local であり、Hosted Native の成功とは扱わない。

## 5. 未実装として明記しているもの

- v2 pixel surface、対話 viewer/gateway、hosted native 実行、Windows の native containment、
  dmg 展開、notarization/stapling 検証、ローカル validator 768MiB と公式上限 1GB の差。
- 本番デプロイ・本番公開は未実施（受け入れ完了後に別承認）。
