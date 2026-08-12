---
title: "Handoff: Static Web Capsule Publication vertical slice — review round 2"
status: handoff
date: "2026-08-04"
related:
  - "PR #1227 (ato, draft, feat/static-web-bundle-producer-v1, rebased on main)"
  - "9 logical PRs across ato / ato-api / ato-pwa / ato-edge"
---

# Handoff: Static Web Capsule Publication — レビュー第2ラウンド完了・最終受け入れ E2E 待ち

この文書は次のセッションのエントリポイントです。最初にこれを読むこと。

## 0. 即ペースト用プロンプト（このまま次のセッションに渡せる）

```
Static Web Capsule Publication 垂直スライスの最終受け入れ E2E を実施する。

## ゴール
「静的SPA ビルド → イミュータブルバンドル → R2/D1/KV → ato-edge CDN → PWA が
static_ready 表示」のエンドツーエンドを、実ビルダー（Docker/Firecracker ホスト）で
通す。Hextris を fixture に使う。ifarme が runner/lease/polling/Stop なしに配信
されることを確認する。

## コンテキスト
4 リポジトリの feature ブランチ（すべて push 済み・レビュー第2ラウンド指摘は全件対処済み）:
- apps/ato       : feat/static-web-publication-pr2 @ 9cfb9971 (スナップショット従来経路も維持)
- apps/ato-api   : feat/static-web-publication-pr2 @ 7510ac1 (migration 0158)
- apps/ato-edge  : feat/static-web-publication-pr6 @ fc55185 (固定 CSP 契約)
- apps/ato-pwa   : feat/static-web-publication-pr8 @ e62f321 (ローカルは main に戻してある)
ローカルは各リポジトリで `git fetch origin && git checkout feat/static-web-*` で復元する。
ato-pwa はローカル main のままでもよい（差分は push 済みブランチにある）。

## 実施手順（下記「Next Steps」§9 に詳細）
1. ビルダーランタイム（Docker ホスト or Firecracker builder）で
   `snapshot-builder` の static-web レーンを起動し、Hextris の dist を実ビルド。
2. `static_web_transport` 経由で prepare/upload-auth/upload/verify/complete を
   実環境（staging D1/R2）に流し、`capsule_static_web_materializations` に
   `ready` 行ができるまで確認。
3. `activateDeployment` を実行し、KV active レコード + D1 active を確認。
4. ato-edge が `s-<digest>.<base>.ato.run` / `p-<label>` で配信することを
   curl + ヘッダー（CSP `frame-ancestors` が固定4オリジン集合）で確認。
5. ato-pwa で該当 capsule を開き、`static_ready` 表示 + iframe 直接配信
   （runner/lease/polling/Stop なし）を目視確認。

## ゴールデンテストベクター（不変）
- manifest digest: sha256:c61c17155f2594c1c32fda225bb5c552d611f5c916b95e904f55afa6b7b69543
- p-l label: p-yyobofk7ewkmdqzp3irfxnofkllbd5ojc24v5ecpkwx2nn5wsvbq
- frame-ancestors 許容集合（固定・この順序）:
  https://ato.run, https://app.ato.run, https://staging.store.ato.run, https://stg-app.ato.run
- テスト実行は vitest を必ずファイル単位で（複数ファイル同時は workerd が不安定）。
```

## 1. 現在地（Where we are）

Static Web Capsule Publication 垂直スライス: 静的SPA を **capability ネゴシエーション
付きのスナップショットビルド**として構築し、イミュータブルバンドル（manifest +
blobs、digest 由来キー）を R2 に置き、D1 に materialization を記録し、ato-edge
CDN が `s-*`（staging）/ `p-*`（production）ホストで配信、PWA が `static_ready`
として表示する。従来の Snapshot 経路（native/rust VM）は **壊していない**（両経路併存）。

レビューは2ラウンド完了。第2ラウンド指摘 **Blocker 6件 + Major 4件は全件対処済み**
（下表）。残りは **実ビルダーでの最終受け入れ E2E のみ**（コード側のシームは
テスト済み・呼び出し配線済み、実ビルドハーネスの実機確認が未実施）。

| リポジトリ | ブランチ | 先頭コミット | 状態 |
| --- | --- | --- | --- |
| apps/ato | feat/static-web-publication-pr2 | 9cfb9971 | コミット済・push 済 |
| apps/ato-api | feat/static-web-publication-pr2 | 7510ac1 | コミット済・push 済 |
| apps/ato-edge | feat/static-web-publication-pr6 | fc55185 | コミット済・push 済 |
| apps/ato-pwa | feat/static-web-publication-pr8 | e62f321 | push 済・ローカルは main に復元済 |

ato の PR #1227（feat/static-web-bundle-producer-v1）は main に rebase + retarget 済み、
**draft のまま**（未マージ）。ato-api / ato-edge / ato-pwa は PR として未作成
（レビュー済みローカルブランチ状態で、PR 化は待機中）。

## 2. 第2ラウンド対処内容（全件完了）

| 指摘 | 対処 | コミット |
| --- | --- | --- |
| B1 書き出し配線 | `ProducedBuild.exported_guest_rootfs` を v1 レーンが実ツリーで設定、clean_replay の static-web 呼び出しがこれを受ける。宣言のみの lane は fail-closed。`export_to` デッドパラメータ削除 | ato 9a336aa3 |
| B2 ジョブフェンシング | ルート jobId = 正確な authoring job に束縛、agentId = job.builderId、status claimed\|running、buildConfigRevisionId 一致、upload/verify/complete は materialization.producerJobId == ルート jobId。`STATIC_WEB_BUILD_ENABLED` は未設定時 fail-closed | ato-api 7510ac1 |
| B3 capsule_id バグ | `capsule_revisions.capsuleId` から実 `capsules.id` を解決（caprev_* を capsule_id に入れない） | ato-api 7510ac1 |
| B4 活性化/停止 CAS | activate の最終 D1 更新は `status='activating' AND activation_generation=<開始時>` の CAS。miss 時は書いた KV レコードを削除。suspend は generation を increment | ato-api 7510ac1 |
| B5 edge 固定 CSP | `STATIC_WEB_FRAME_ANCESTORS_V1` 固定集合（上記4オリジン・順序込み）以外は拒否、connect_src はソート+ユニークを強制。3リポジトリでバイト一致 | ato-edge fc55185 |
| B6 migration 番号 | 0157 → **0158** に変更（#470 の 0155、#475 の 0155+0156 と衝突回避） | ato-api b3e062e |
| M1 重複コンポーネント | component-walk を順序保証で蓄積（a/a/dist が symlink の2つ目の a をスキップしない） | テスト固定 |
| M2 412 ハンドリング | create-only PUT の 412 = 既存 → verify 経路へ収束（リトライ燃焼しない） | ato 9cfb9971 |
| M3 `%` 拒否 | オーサリング root/entry_path と Rust manifest validator 双方で `%` 拒否（プロデューサ/API パリティ） | ato 9cfb9971 |
| M4 staging 適格性 | staging は public + secretless + **free（price===0）** + **stateless（保存済み TOML に `[state.<name>]` なし）** のみ | ato-api 7510ac1 |

## 3. テストステータス（最終検証済み・全グリーン）

| 対象 | コマンド | 結果 |
| --- | --- | --- |
| Rust (capsule/snapshot/snapshot-builder) | `cargo test -p capsule -p snapshot -p snapshot-builder` + `cargo clippy ... -D warnings` | 全パス・clippy クリーン |
| ato-api 主要スイート | 各ファイル個別に `npx vitest run` | deployment 10 / registry 3 / wizard 53 / contract 9 / build-plan 14 / preview 39 = 128 パス |
| ato-edge | `npm test` | 19/19 |
| ato-pwa | `npm test` | 1244/1244 |

注意: vitest は**複数ファイル同時実行が workerd 起因で不安定**（loupe /
EADDRNOTAVAIL / TLS alert）。必ず1ファイルずつ実行すること。

## 4. 主要ファイル

### apps/ato（Rust）
- `crates/snapshot-builder/src/main.rs` — `ProducedBuild.exported_guest_rootfs`、clean_replay の static-web 呼び出し
- `crates/snapshot-builder/src/static_web_output.rs` — component-walk（symlink 脱出修正 + 重複名テスト）
- `crates/snapshot-builder/src/static_web_bundle.rs` — 境界付き読み + fstat inode/size、digest→size 一意、`%` 拒否
- `crates/snapshot-builder/src/static_web_emit.rs` — 書き出し
- `crates/snapshot-builder/src/static_web_transport.rs` — prepare/upload-auth/upload/verify/complete、412 収束、digest 由来キー、URL 難読化、資格情報なし
- `crates/snapshot-builder/src/authoring_runtime.rs` — `AuthoringWork` プラン項目、capability 広告
- `crates/capsule/src/foundation/types/manifest_v1.rs` — `%` 拒否（プロデューサ/API パリティ）

### apps/ato-api（Cloudflare Workers）
- `drizzle/0158_static_web_materializations.sql` — 3テーブル（materialization / build attempts / revisions）+ 関連
- `src/services/static_web/registry.ts` — prepare/upload-auth/verify/complete + フェンシング、`StaticWebRegistryError` httpStatus は 400|403|404|409|503
- `src/routes/static_web_artifacts.ts` — ルートハンドラ（jobId+agentId）、fail-closed `STATIC_WEB_BUILD_ENABLED`
- `src/services/static_web/deployment.ts` — createProductionDeployment / createStagingDeployment（適格性）/ activateDeployment（世代 CAS）/ suspendDeployment / suspendDeploymentsForCapsule / reevaluateActiveDeployments
- `src/services/submission_wizard/authoring_builder_runtime.ts` — capability ゲート付きプラン項目（supportsStaticWeb）
- `src/tests/static-web-deployment.test.ts`（10）/ `static-web-registry.test.ts`（3）

### apps/ato-edge
- `src/manifest.ts` — `STATIC_WEB_FRAME_ANCESTORS_V1` 固定集合 + connect_src ソート/ユニーク
- `test/core.spec.ts` — ゴールデンベクター + 固定フレームテスト（19/19）

### apps/ato-pwa
- `static_ready` 状態、discriminated `PreviewRunStart`、`normalizeStaticWebDelivery`、openCapsuleClient 静的結果、`buildStaticContentUrl` / `isPublicStaticHostLabel`、Stop なし `PreviewPlayer`、AppSessionDetailsModal 静的セクション、`reportInstallLocation` static-web kind（1244/1244）

## 5. ゴールデンテストベクター（不変・3リポジトリで一致させる）

- manifest digest: `sha256:c61c17155f2594c1c32fda225bb5c552d611f5c916b95e904f55afa6b7b69543`
- p-l label: `p-yyobofk7ewkmdqzp3irfxnofkllbd5ojc24v5ecpkwx2nn5wsvbq`
- frame-ancestors 許容集合（固定・この順序・これ以外は拒否）:
  `https://ato.run`, `https://app.ato.run`, `https://staging.store.ato.run`, `https://stg-app.ato.run`
- connect_src はソート済み + ユニーク必須

## 6. 設計上の重要な決定（変更時は要理解）

- **スナップショットと並存**: static web は既存 Snapshot パイプラインの別レーン（producer contract `ato.static-web-producer/v1`）。従来経路のレグレッションなし。
- **フェンシング（B2）**: ビルダーagent の共有トークンが別ジョブの materialization を操作できない。ジョブID・agentId・revision・producerJobId の4重束縛。fail-closed。
- **活性化の世代 CAS（B4）**: suspend と activate の競合で、停止済み capsule が古い活性化で再公開されない。
- **CSP は edge で強制（B5）**: プロデューサ/API が受け取る manifest の frame-ancestors は無視せず、edge が固定集合を強制。
- **`%` は全境界で拒否（M3）**: オーサリング（API プラン検証）と Rust manifest validator で一致。ログの URL は難読化、資格情報は送らない。
- **staging は「public + free + stateless + secretless」のみ（M4）**: `price > 0` や `[state.]` があれば staging 対象外。

## 7. ブランチ運用の注意

- ルート `ato-run/` は git リポジトリでない。必ず対象アプリのディレクトリで git 操作する。
- ato / ato-api / ato-edge は**ローカルが feature ブランチのまま**。次セッションはそのまま作業可。
- ato-pwa は**ローカル main に復元済み**。feature 差分は `feat/static-web-publication-pr8` に push 済み（e62f321）。必要なら `git fetch origin && git checkout feat/static-web-publication-pr8`。
- ato の PR #1227 は draft。レビュー指摘が閉じたので、次の手は「draft 解除 → レビュー依頼」または「リベース後再依頼」の判断が必要。
- コミットは author `koh0920`、Co-Authored-By なし。

## 8. 既知のハマりどころ

- **vitest 複数ファイル同時実行は不安定** → 必ず `npx vitest run <file>` 単位で。
- `capsule_build_config_revisions` は**イミュータブルトリガー付き**（UPDATE 不可）。テストで内容を変える場合は別行 INSERT して materialization を差し替える（static-web-deployment.test.ts 参照）。
- テストシードに `bcrev_1` の build config revision 行が必要（deployment.test.ts のヘルパーに seed 済み）。
- ato-edge の push 先は `github.com:ato-run/ato-contents.git`（リポジトリ名が ato-edge でない）: `git remote -v` で確認。

## 9. Next Steps（次のセッションのTODO）

1. **実ビルダー E2E（残タスクの唯一の本体）**
   - Docker ホスト or Firecracker builder（Sugamo 等）で `snapshot-builder` の static-web レーンを起動し、Hextris の dist を実ビルドして `dist` → bundle → upload を通す。
   - staging 環境（staging D1/R2）で prepare → upload-auth → upload → verify → complete を実行し、`capsule_static_web_materializations` に `ready` 行が立つまで確認。
   - `activateDeployment` を実行し、KV active レコード + D1 active を確認。
   - ato-edge が `s-*` / `p-*` ホストで配信すること、CSP ヘッダーが固定集合であることを curl 確認。
   - ato-pwa で開き、`static_ready` + iframe 直接配信（runner/lease/polling/Stop なし）を確認。
2. **PR 化判断**: ato PR #1227 の draft 解除 or 再レビュー依頼。ato-api / ato-edge / ato-pwa の PR 作成（レビュー結果は反映済み）。
3. **マージ後の動作確認**: ato.run / app.ato.run で表示確認（既存の ato-pwa デプロイ運用に従う）。

## 10. 参考

- 仕様: `apps/ato/docs/rfcs/accepted/`（CAPSULE_SPEC / NACELLE_SPEC / SNAPSHOT 系）、ato-api は `apps/ato-api/docs/rfcs/`
- 本スライスのレビュー第1ラウンドは全件解決済み（この handoff の親セッション履歴に詳細）
- テストコマンド集は各リポジトリ AGENTS.md 参照
