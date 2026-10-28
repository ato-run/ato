# PWA home/About 改善の統合（#561 / #563 / #565）— Portable Capsule v1 追補

状態: 草案（Draft、2026-10-28 JST）。staging で観測済み、Production へは未適用。
対象: ato-pwa staging の home/About 変更と、Portable Capsule v1 の staging 検証・配備ピンとの関係。
この文書は実装・設定・データを変更しない。記載は staging と現行コードの観測に限る。

## 0. 位置づけと根拠

> 本文中の `.tmp/...` は作業記録のローカルパス（リポジトリ外）であり、恒久的な参照先ではない。恒久記録はこの文書と各 PR/commit である。

- 統合候補ピン（観測時点、2026-10-28T08:14Z 前後）: PWA staging Worker `5ffddf63-d2d6-4f0e-9743-dcc794c9ecef` / asset `assets/index-CGL6ctxK.js` / build `fb475bb`（stg `fb475bba`）、API staging `914ad323-335f-4b91-8463-02b44cd518ed`（stg `d95fab7f`）、ato stg `da9afc4f8`、runner `9fbfe8f7`（6 slot）。
- 変更 PR: ato-pwa #561（merge `27ed1a0c`、base `stg`）、#563（`f188d861`）、#565（`fb475bba`）。stg の現 tip は `fb475bba`。
- 観測証跡: `.tmp/portable-v1-stabilize/integration/home.html` / `home.headers` / `about.html` / `about.headers` / `index-CGL6ctxK.js` / `pwa-integration-evidence.json`。配備読み戻しは `.tmp/shenzhen-public-evidence/staging-verify-about-review.log`、`.tmp/shenzhen-public-evidence/staging-deploy-about-review.log`。
- 本節は PWA の表示・配信のみを扱う。Portable Capsule の K/D、validator、receipt の権威は従来どおり Rust 側にあり、本変更はそれらを変更しない。
- 本節は `docs/ops/portable-capsule-v1-scope-20261028.md` の §5.4 と同じ主題を、独立した追補として観測時点の build で整理したものである。両者が食い違う場合は、より新しい観測である本節の値（PWA `5ffddf63` / `assets/index-CGL6ctxK.js` / build `fb475bb`、API `914ad323`）を優先する。§4 の同一 build 検証は staging-e2e の実行結果（21/21 PASS、`.tmp/portable-v1-stabilize/integration/REPORT.md`）で確定した。
- この文書が主張しないこと: Production の状態、検索エンジンでの実際の snippet 結果、同一 build での Portable Capsule 回帰の合否（§4 の FILL）、preview 環境の runtime 実行、Release Manifest の一致。

## 1. 結論（1 段落）

PWA home/About の 3 変更（#561 / #563 / #565）は stg `fb475bba` に含まれ、staging では Worker `5ffddf63` / asset `assets/index-CGL6ctxK.js` として配信されている（served JS に build 文字列 `fb475bb` を含むことを実測）。home では前置きと title/meta が配信 HTML に現れ、カード概要は `data-nosnippet`（213 箇所）で snippet から除外されつつアプリへのリンクは crawlable のまま、`/about` は 307 → `/about/` 200 で表示される。API は `914ad323` のまま不変で、Portable Capsule の import/export 経路（API と Rust authority）には差分がない。同一 build での Portable Capsule 回帰は **21/21 PASS**（classic Add は正確に 409、portable 2 件は two-account で import→export→delete→B import→app 200・K 一致）。Production へは未配備で、昇格には §6 のゲート（pinned manifest E2E、API release と deploy 順序）が必要である。

## 2. 変更内容（PWA）

### 2.1 #561（merge `27ed1a0c`、fix/home-search-snippet）

| 変更 | 内容 | コード/テスト根拠 |
|---|---|---|
| SEO snippet 境界 | Discover のカード概要を `data-nosnippet` の `<span>` にし、Google がホームの snippet にアプリ概要を流用しないようにする。アプリへのリンク（`/?view=discover&detail=...`）とアプリ詳細説明は維持 | `src/components/personal-apps/discovery/DiscoverFeedCard.tsx:110`、DOM test `DiscoverFeedCard.dom.test.tsx:23`、prerender `worker/publicPages.ts:254`、E2E `e2e/home-seo.pw.ts:26`（description が nosnippet かつリンクが crawlable であることを assert） |
| home 前置き | prerender の home に `<h1>Explore apps on Ato</h1><p>Discover open-source apps …</p>` を出し、catalogue API 不達でも前置きを表示。shell 側は title/meta/og/twitter に同文面を持つ | prerender `worker/publicPages.ts:254`、テスト `worker/publicPages.test.ts:149,202,218`、shell `index.html:21,25,32,34`、meta テスト `scripts/home-meta.test.mjs:11` |
| query-fenced first-load reads | 最初の画面が読み込む read（`GET/HEAD` の `/api/auth/get-session`、`/v1/client-compatibility`、`/v1/discover/*`）と startup POST（`/v1/discover/entry`、`/v1/discover/apps/:id/start`）は、custom header ではなく **query** に client type/contract/build を載せて CORS-simple のまま扱う（preflight を増やさない） | `src/app/clientContract.ts:170-171`（定義）、`:224-237`（query への付与と header 経路の分岐） |
| clean-home fence | hero preload・パネル復元・locale 先読みは「pathname が `/` で、query が hero / view=discover のみ」のときだけ実行 | `src/main.tsx:69-74`、`build/heroPreload.ts:41`（`location.pathname!=="/"||location.search` で return）、`src/components/shell/DiscoveryPanels.tsx:20` |
| CI / deploy guard | CI を self-hosted ephemeral runner で実行し、staging deploy guard を Vitest 化。削除済み Activity UI の workflow を廃止し、home SEO 境界の明示的な browser test を追加。staging は lock し、production は main からの別 deploy を要求 | PR #561 body の Validation/Release constraints。CI run `40638521725` / home SEO `40638521749`（同 body） |

### 2.2 #563（`f188d861`）

- script-free の英語/簡体字中国語 About ページを追加（`src/static-site/About.tsx`）。共通 footer リンク、search metadata、sitemap を追加し、既存 Docs 各 locale の current-status / CLI quickstart を修正（`content/docs/<locale>/{what-is-ato,cli-quickstart}.mdx`）。
- 公開情報（Shenzhen）関連の staging 配信記録を `docs/ops/public-information-shenzhen-2026-10-28.md` に追加。
- 検証は Node 22 typecheck、unit 358 files / 3,187 tests、build、home SEO E2E がローカルで pass。About は JavaScript 無効の desktop/mobile で確認（独立した中国語レビュー・初回読解の時間計測は未主張）。

### 2.3 #565（`fb475bba`）

- About の可読性と computation model の説明を改善（`src/static-site/About.tsx`、`public/static-site.css`）。K/D より前に「保全と継続」という問題と Capsule モデルを示し、2048 の demo を Discover/Execution に接続し、利用可能・開発中・研究段階を区別する（en / zh-CN）。
- diagram は概念図であり universal portability を主張しない。2048 は特定の Contract/Derivation/receipt の証拠として提示しない（PR #565 body）。
- 検証: typecheck/build pass、static content/Docs layout/Worker routing/robots の 80 tests pass、320/390/1440px・JS 無効で overflow と link を確認。

## 3. staging 観測（2026-10-28、read-only）

| 観測項目 | 結果 | 証跡 |
|---|---|---|
| PWA 配信バージョン | Worker `5ffddf63-d2d6-4f0e-9743-dcc794c9ecef`（100%）、API `914ad323`（100%） | `.tmp/shenzhen-public-evidence/staging-verify-about-review.log:2,8` |
| 配信 asset / build | `assets/index-CGL6ctxK.js`、515,312 bytes、served JS に `fb475bb` を含む | `integration/index-CGL6ctxK.js`、`staging-deploy-about-review.log:124,263` |
| home HTTP | 200、81,868 bytes | `integration/home.html`、`curl -w` 記録 |
| home title / description | title `ato.run — Discover what’s open`、description に新前置き | `integration/home.html`、`index.html:21,34` |
| home 前置き（prerender） | `<h1>Explore apps on Ato</h1><p>Discover open-source apps …</p>` | `integration/home.html` |
| `data-nosnippet` | **213 箇所**。カード概要は `<span data-nosnippet>— {description}</span>`、リンクは `/?view=discover&detail=…` で crawlable | `integration/home.html`、`DiscoverFeedCard.tsx:110` |
| About | `/about` → **307**（`location: /about/`）→ `/about/` **200**、10,848 bytes、title `About Ato and ato.run \| Ato`、`lang=en`、hreflang en / zh-CN / x-default | `integration/about.headers`、`integration/about.html` |
| robots（staging） | `x-robots-tag: noindex, nofollow`（home / about とも） | `integration/home.headers`、`integration/about.headers` |
| API | `914ad323` のまま不変（PWA 配備中も version 不変） | `.tmp/shenzhen-public-evidence/staging-verify-about-review.log:8`、scope doc §5.2 |

補足: 配備は並行タスク（about-review）が staging deploy lock（`release=fb475bba`）を取得して実施し、検証後に `LOCK_RELEASED`（`staging-deploy-about-review.log:5,264`）。#561 自身の post-merge 自動 deploy は GitHub の課金制限で開始前に失敗しており、この配備は #561 の pipeline によるものではない。

## 4. Portable Capsule フローとの相互作用

- **API / Rust authority は不変**: 本 3 変更は ato-pwa のみ。#561 の PR body も「staging API は `90b277ae` のまま」としており、観測時の API は `914ad323`（#945 後）で、Portable Capsule の import/export、validator、receipt の経路に差分はない。
- **Discover / Library の import/export**: 画面上の変更は「カード概要の snippet 除外」「home 前置き」「`/about` の静的ページ」に限られる。export ボタンの表示条件は従来どおり `portable_export_available`（その Instance の portable import が verified か）で決まり（`api/src/routes/compute_instances.ts:1130-1131`、PWA `AppTab.tsx:147`）、本変更はこの判定に触れない。`/about` はアカウント面ではなく、公開静的ページである。
- **同一 build での M3 回帰**: 本変更を含む build で、Portable Capsule の代表フローが成立するかは staging-e2e が記入する。**未実施**（下記 FILL）。API 不変のため理論上の影響は小さいが、PWA build が変わったこと自体は E2E の前提条件である。
- **with-data との関係**: 保存データ付き export の capture fence（ato-api #946、`dynamic_state/writer.ts` / `portable_instance_export.ts`）は API 側の変更であり、PWA home/About とは独立。本節の変更で #946 の検証状況は変わらない。

| 同一 build での確認 | 状態 | 出所 |
|---|---|---|
| plaza（classic Add / export 409 経路） | **PASS_NOT_EXPORTABLE**: library add 200、保存参照 1、owned Instance 0、publisher compute open 404、export probe = 正確に **409 `portable_export_source_unavailable`**（2xx は FAIL とする false-PASS guard 付き） | staging-e2e / `integration/evidence/` |
| memos（two-account / portable） | **PASS**: import verified・receipt `fully_satisfied=true`（attempt `pav_01M60SWT9…`）、app 200、app-only export v4 で K `64368f74…` 保持、A instance 削除（202→200→404）、B import verified、B app 200、K 一致、cross-account 404 | 同上 |
| databag（two-account / portable） | **PASS**: K `500d85d4…`、同経路で B import・app 200・K 一致 | 同上 |
| 2048（static Browser） | **UNVERIFIED_STATIC_ONLY**: 静的 entry 200、portable release 未作成のため Export→Import 不可（M2 の最初の変換対象） | 同上 |
| Discover / Library の表示回帰 | **PASS（対象 3 listing + 2048 の範囲）**: 一覧表示・保存・詳細の描画に回帰なし。screenshots を `integration/screens/` に保存。全 213 listing の表示回帰は未実施 | staging-e2e |

## 5. 統合候補ピン（この節の観測時点）

| 層 | ピン | 根拠 |
|---|---|---|
| PWA | stg `fb475bba`（#561 `27ed1a0c` / #563 `f188d861` / #565 `fb475bba` を含む） | `git -C apps/ato-pwa log -1 origin/stg`、PR merge commit |
| PWA 配備 | Worker `5ffddf63-d2d6-4f0e-9743-dcc794c9ecef`、asset `assets/index-CGL6ctxK.js`、build `fb475bb` | verify/deploy log、served asset の `fb475bb` |
| API | staging `914ad323-335f-4b91-8463-02b44cd518ed`、stg `d95fab7f` | verify log、scope doc §5.2/§5.3 |
| ato | stg `da9afc4f8`（#1615） | Lead 提供（本節では再検証せず） |
| runner | `9fbfe8f7`（6 slot active） | scope doc §5.2、Lead 提供 |
| Release Manifest | **この組み合わせを固定した pin は無い**（既存 manifest は staging より遅れており STALE/MISMATCH） | scope doc §5.2 の Manifest 照合 |

## 6. 残ゲート（Production 昇格まで）

1. **Production deploy**: PWA の production deploy は未実施（main からの別デプロイが必要）。API も production へは未変更。本節の観測は staging のみ。
2. **pinned-manifest E2E**: 上記の組み合わせを Release Manifest に固定し、その pin の下で Portable Capsule の代表 E2E（同一 build の M3、app-only / with-data、negative）を実行する必要がある。現状は pin が無い。§4 の同一 build M3 は PASS 済みだが、pin 固定下での再実行は未実施。
3. **production contract diff**:
   - **read 経路 = PASS**（read-only probe）: production API は query fence を実装済みで、`/v1/client-compatibility` は `{api_contract:1,min_pwa_contract:1}`、`__ato_client_*` query 付き `/v1/discover/apps(/page)` は 200（routing 前に params を strip）、`__ato_client_contract=0` で 426 `pwa_update_required`、`/api/auth/get-session` も同様。payload shape は prod/stg で一致（staging のみ `ai_rating`、PWA 型で optional）。
   - **portable import payload = 既知 mismatch / UNVERIFIED**: production API 実 version `80ac799c-5de0-4cbd-a305-a3c4902bca93`（commit `532032accd33`、hotfix branch）の strict portable import schema は `accepted_session_surfaces` / `compute_account` を受理せず、staged API（`90b277ae` 以降）は受理する（PR #561 body）。コード確認では `origin/main` は `accepted_session_surfaces` を持つが `compute_account` は持たず、stg のみが持つ。したがって production 昇格には **API release（main 由来）→ PWA deploy の順序**、`compute_account` の main 取込または互換確認が必要。
   - production API の実 build は hotfix branch 由来で main の祖先ではないため、manifest pin で扱う（祖先推定をしない）。
4. **preview runner 未使用**: #561 の preview（`dev-561-eeb82c7`）は isolated D1/R2/KV と API health、PWA HTML/JS の確認までで、isolated Runner を provisioning しない。runtime 実行は未検証で、self-hosted CI VM は nested KVM を提供できず Runner の代用にならない（PR #561 body）。
5. **CI 課金**: #561 の CI（`40638521725` / `40638521749`）は self-hosted runner で pass したが、merge 後の自動 staging deploy は GitHub の課金制限で開始前に失敗した。課金制限による失敗を成功として扱わない。
6. **SEO 実効性**: staging は `noindex, nofollow` のため、`data-nosnippet` の実際の検索結果への効果は観測できない。確認できたのは HTML 属性とリンク構造まで。

## 7. UNVERIFIED / NOT RUN 一覧

| 項目 | 状態 | 理由 |
|---|---|---|
| Production PWA / API の配備 | **NOT DEPLOYED** | staging のみ。production は別デプロイ・別承認 |
| この組み合わせを固定した Release Manifest | **NOT PINNED** | manifest は編集していない。既存 pin は staging より古く STALE/MISMATCH（scope doc §5.2） |
| pinned-manifest 下の Portable Capsule E2E | **NOT RUN** | §6-2。同一 build の M3 は PASS 済み（§4）だが、pin 固定下の再実行は未実施 |
| 同一 build の M3（plaza / memos / databag / 2048 / 表示） | **PASS（21/21）** | 同一 deployed identity で PWA と Portable 回帰を検証。identity は前後で不変 |
| production API との契約互換 | **PARTIAL**: read 経路は PASS / portable import payload は既知 mismatch（UNVERIFIED） | read-only probe（read 経路）と PR #561 body + branch コード確認（import payload）。`accepted_session_surfaces` は main にあり、`compute_account` は stg のみ |
| preview 環境での runtime 実行 | **NOT VERIFIED** | isolated Runner なし。nested KVM 不可（PR #561 body） |
| 検索エンジンでの snippet 実結果 | **NOT VERIFIED** | staging は `x-robots-tag: noindex, nofollow` |
| About の en / zh-CN 以外の言語 | **NOT APPLICABLE** | About は en / zh-CN のみ。#563 は Docs locale の修正を含むが About の追加言語は無い |
| About の独立中国語レビュー・初回読解時間計測 | **NOT CLAIMED** | PR #563 body が明示 |
| 完全な Runner 集合 / Release Manifest の一致 | **UNVERIFIED** | scope doc §5.2 のとおり、Runner は混在・一部欠落 |
| 本節の観測の再実行（現行 build での追加確認） | **NOT RUN** | 観測は 2026-10-28T08:14Z 前後の 1 回。以後の build 変更は追跡していない |

## 8. 再現（read-only）

```sh
cd /Users/egamikohsuke/Ekoh/projects/ato-run
I=.tmp/portable-v1-stabilize/integration
# home（title/meta/前置き/data-nosnippet 213 箇所/asset 名）
curl -sS -L --max-time 30 -4 -D $I/home.headers -o $I/home.html https://stg-app.ato.run/
grep -o "data-nosnippet" $I/home.html | wc -l            # 213
grep -o "<title>[^<]*</title>" $I/home.html | head -1
grep -o "assets/index-[A-Za-z0-9_-]*\.js" $I/home.html | sort -u
# about（307 -> /about/ 200）
curl -sS -o /dev/null --max-time 25 -4 -D $I/about.headers https://stg-app.ato.run/about   # HTTP/2 307, location: /about/
curl -sS -L --max-time 25 -4 -o $I/about.html https://stg-app.ato.run/about
# 配信 JS の build 文字列
curl -sS --max-time 60 -4 -o $I/index-CGL6ctxK.js https://stg-app.ato.run/assets/index-CGL6ctxK.js
grep -c fb475bb $I/index-CGL6ctxK.js
# 配備・version の読み戻し（並行タスクの証跡）
grep -n "version_id\|Current Version ID\|LOCK_" .tmp/shenzhen-public-evidence/staging-verify-about-review.log .tmp/shenzhen-public-evidence/staging-deploy-about-review.log | head
# PR
gh -R ato-run/ato-pwa pr view 561 --json number,state,mergeCommit,body
```

コード参照は ato-pwa `origin/stg` = `fb475bba` の内容。行番号はこの commit 時点のものである。

## 9. 追補: 検証後の staging build 前進（2026-10-28T08:40Z）

本文 §3〜§5 の観測後、共有 staging では別タスクが PWA を再度配備した。**検証済みの pin は 2 つあり、混同しない**。

| 検証 | PWA Worker / asset / build | stg | 結果 |
|---|---|---|---|
| 第1回（本文 §3） | `5ffddf63-d2d6-4f0e-9743-dcc794c9ecef` / `assets/index-CGL6ctxK.js` / `fb475bb` | `fb475bba`（#561/#563/#565） | **PASS 21/21** |
| 第2回（本追補） | `1b713b1a-70e6-4bdb-9d9b-dcc5732a3618` / `assets/index-Dx8OzJEF.js` / `9db56a1` | `9db56a1d`（#566 feat/about-imagen-review。`#561/#563/#565` を ancestor に含む） | **PASS 21/21** |

- 第2回は API `914ad323` / stg `d95fab7f` 不変のまま、identity を前後で確認（mid-run 変更なし）。
- 第2回で追加確認した点: #566 の About 画像 3 種（icon PNG 148,409B / about-capsule-continuation.webp 45,650B / about-tile-puzzle-illustration.webp 55,914B）がすべて 200 `image/*` で、Playwright 上も `complete` かつ `naturalWidth > 0`（broken image 0）。home の `data-nosnippet` は 213 のまま、crawlable link も 213 のまま。
- 第2回の Portable 回帰（同一 build）: `discover-plaza` = PASS_NOT_EXPORTABLE（正確に 409）、`discover-portable-memos` = PASS（receipt `fully_satisfied=true`、attempt `pav_01M60TQNQSPE3EJRNB4VN1P26C`、K `64368f74…` 保持、B import・app 200・K 一致、cross-account 404）。
- 証跡: `.tmp/portable-v1-stabilize/integration/latest-build/{REPORT.md,verdict.json,evidence/,screens/}`（第2回）、`.tmp/portable-v1-stabilize/integration/`（第1回）。

**注意**: 2 回の PASS はそれぞれ別 build の観測であり、build 間の等価性を主張しない。共有 staging は今後も前進し得るため、Release Manifest へ固定する際は「どの build を検証したか」を pin と一緒に記録する。production は依然未配備で、§6 のゲート（API release と deploy 順序、pinned-manifest E2E）は未解消のままである。
