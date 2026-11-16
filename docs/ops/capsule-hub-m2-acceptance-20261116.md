# Capsule Hub M2 — 受け入れサマリ（2026-11-16）

Status: 実装・staging 公開・大容量 publish 受け入れまで完了。実機 native 2 D 検証と staging PWA 反映が未了。
Parent: ato #1819（重複 Issue なし）。契約: `docs/rfcs/draft/capsule-hub-m2-native.md`。

## 1. マージ済み PR（dev → stg 到達済み）

| 領域 | PR | 内容 |
| --- | --- | --- |
| API | ato-api #1287/#1288 | 公式 publisher 限定 1GB publish（migration 0419、presigned R2、complete 検証、sweep） |
| API | ato-api #1289/#1290 | 大容量 publish の staging 受け入れ記録 |
| API | ato-api #1291/#1292 | 再現可能な staging E2E スクリプト |
| API | ato-api #1836/#1837（ato）同等の publish null metadata 修正は ato 側 | — |
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

## 2. 実測済み（staging）

- 大容量 publish（`scripts/staging/capsule_hub_m2_large_publish_e2e.sh`）: **11/11 pass**
  — 公式 12.6MB の presigned PUT、未認証 resolve/artifact/download-url、一般 8MiB 超 413、
  特権偽装 403 `privilege_not_assertable`、非所有 namespace 403、1GB+1 413、
  未完了 complete 404 かつ version/tag なし、R2 checksum 不一致 403。
- `capsules/ato/solidcraft` 公開: `sha256:1f1895ec…`、2 derivations、`native_terminal`、
  `requires.platforms=["macos/aarch64"]`、`verification=experimental / 0 runs`（成功 Run のみ数える）。
- ページ `https://stg-app.ato.run/capsules/ato/solidcraft` は 200 で描画。

## 3. 未了（この環境では実行できない）

1. **実機 2 D の K 充足**: harness が Seatbelt を適用できず（`sandbox_apply: Operation not permitted`）、
   `danger-full-access` 承認もタイムアウト。`scripts/acceptance/verify-solidcraft-native.sh` を
   Seatbelt が使える端末で実行する。
2. **staging PWA への「実行方法」反映**: 共有 lease 取得に Cloudflare 認証が必要
   （OAuth 失効、残 token は D1 権限なし `code: 7403`）。`wrangler login` 後に `npm run deploy:staging`。

## 4. 未実装として明記しているもの

- v2 pixel surface、対話 viewer/gateway、hosted native 実行、Windows の native containment、
  dmg 展開、notarization/stapling 検証、ローカル validator 768MiB と公式上限 1GB の差。
- 本番デプロイ・本番公開は未実施（受け入れ完了後に別承認）。
