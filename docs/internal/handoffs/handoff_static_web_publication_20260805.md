---
title: "Handoff: Static Web Capsule Publication vertical slice — review round 8 complete, E2E & merge pending"
status: handoff
date: "2026-08-05"
related:
  - "PR #1227 (ato, draft, feat/static-web-bundle-producer-v1)"
  - "4 repos: ato / ato-api / ato-edge / ato-pwa feature branches"
---

# Handoff: Static Web Capsule Publication — レビュー8ラウンド完了・最終受け入れ E2E 待ち

この文書は次のセッションのエントリポイントです。最初にこれを読むこと。

## 0. 即ペースト用プロンプト（このまま次のセッションに渡せる）

```
Static Web Capsule Publication 垂直スライスの最終受け入れを実施する。

## ゴール
1. ato-api を最新 main へ rebase（main は capsule-import-v3 / PR #475 で再進行中、
   migration 0156/0157 を追加）。static-web migration 0158/0159/0160/0161 の番号衝突を確認。
2. Hextris real-builder E2E: 実ビルダー（Docker/Firecracker）で snapshot-builder の
   static-web レーンを起動し、「real build → dist → guest rootfs export → bundle →
   prepare/upload/verify/complete → s-* → publish → p-* → PWA iframe 表示」を通す。
3. PR 化: ato PR #1227 draft 解除 or 再依頼。ato-api / ato-edge / ato-pwa の PR 作成。

## 前提（全て検証済み・変更しない）
- 4 リポジトリの feature ブランチ（push 済み）:
  - apps/ato      : feat/static-web-publication-pr2 @ 57497fe8
  - apps/ato-api  : feat/static-web-publication-pr2 @ 00e1306（最新mainへ rebase が必要）
  - apps/ato-edge : feat/static-web-publication-pr6 @ a30994a（リモートは ato-contents.git）
  - apps/ato-pwa  : ローカル main のまま（差分は pr8 @ e62f321 に push 済み）
- ローカルは各リポジトリで `git fetch origin && git checkout feat/static-web-*` で復元。
- ato-api 作業ツリーにユーザーWIP（capsules.ts）が未コミットで残っている。触らず、
  commit へ混ぜない。rebase 時は stash → rebase → pop。
- テストは vitest を必ずファイル単位で（複数ファイル同時は workerd が不安定）。

## 実施手順
1. ato-api rebase: `git fetch origin` → main との差分確認（capsule-import-v3 が
   confirm_publish に触れていないか確認）→ `git rebase -X theirs origin/main` →
   confirm_publish.ts に static-web 統合（secret分類/拒否/ensure）が残っているか確認。
   migration 0156/0157 と 0158/0159/0160/0161 の衝突なしを確認。
2. 関連 suite をファイル単位で実行（下記テストコマンド）。baseline（origin/main 同一）の
   red は submission-wizard-routes 6件のみ。
3. Hextris E2E（下記 §Next Steps 詳細）。

## ゴールデンテストベクター（不変）
- manifest digest: sha256:c61c17155f2594c1c32fda225bb5c552d611f5c916b95e904f55afa6b7b69543
- p-l label: p-yyobofk7ewkmdqzp3irfxnofkllbd5ojc24v5ecpkwx2nn5wsvbq
- frame-ancestors 許容集合（固定・この順序）:
  https://ato.run, https://app.ato.run, https://staging.store.ato.run, https://stg-app.ato.run
```

## 1. 現在地（Where we are）

Static Web Capsule Publication 垂直スライス: 静的SPA を **capability ネゴシエーション付きの
スナップショットビルド**として構築し、イミュータブルバンドル（manifest + blobs、digest 由来キー）
を R2 に置き、D1 に materialization を記録し、ato-edge CDN が `s-*`（staging）/ `p-*`
（production）ホストで配信、PWA が `static_ready` として表示する。従来 Snapshot 経路（native/rust VM）
は両経路併存で維持。

レビューは **8ラウンド完了**。各ラウンドの Blocker/Major は全て対処済み。残りは
**①最新 main への再 rebase（main が再進行）②Hextris real-builder E2E ③PR 化** のみ。

| リポジトリ | ブランチ | 先頭コミット | 状態 |
| --- | --- | --- | --- |
| apps/ato | feat/static-web-publication-pr2 | 57497fe8 | push 済 |
| apps/ato-api | feat/static-web-publication-pr2 | 00e1306 | push 済・main 遅れ |
| apps/ato-edge | feat/static-web-publication-pr6 | a30994a | push 済 |
| apps/ato-pwa | main（pr8 は e62f321 に push） | — | 変更不要 |

## 2. ラウンド別対処サマリ（全て承認済み）

### Round 1-2（第1・2次レビュー）
- agent_id/worker_claim_id/lease header の全 API 呼び出し伝播、contract test
- blob closure テーブル + closure 正規化（authorize/verify は closure 内のみ、complete は closure join）
- dedup 含む batch 全体の再 verify
- authoring lease を利用した job fencing、terminal owner からの generation handover
- eligibility 一元化（staging/production/resolver/cron）、fixed 4-origin CSP

### Round 3
- closure_digest（JCS sorted digest）を materialization 同一 INSERT で確定、部分 closure repair
- complete の owner/generation/status CAS + fresh row 再取得
- `secretsRequired !== false` と manifest secret config で fail-closed
- `clean_replay` operation 限定、Rust response 検証（count/digest/duplicate/skew）

### Round 4-6
- activation 直前 eligibility 再検証（activateDeployment 内部 + cron desired/activating 対応）
- CAS 敗者の KV 削除競合（activationWonElsewhere 共通 helper、probe-failure rollback-CAS-first）
- wizard secret classification（Program Intent トップレベル bindings を共通 schema で parse）
- `secret_binding_required` publish 拒否、publish 後 postcondition に新分類を伝播
- publish retry convergence（fresh/retry 両経路で deployment ensure）
- seal identity 誤比較削除（snapshotId vs currentReadyStateSealId）、実 prefix fixture 化
- suspendDeployment 真の generation CAS + retry exhaustion 時は例外（KV 不触）

### Round 7（main rebase + publish 再統合）
- `-X theirs` で最新 main へ rebase（0 behind にしたが、その後 main が再進行）
- main の studio_version CAS を唯一の publish commit point として維持
- secret 分類/拒否は CAS 前、`secrets_required` は単一 Capsule CAS に統合
- fresh/convergence とも postcondition 通過後に deployment ensure
- `publish_state_incomplete`/CAS 競合では deployment を作成しない
- detector-provenance 残骸除去（main は transient detector のみ保持）、migration 0155 衝突解消

### Round 8（最終レビュー）
- **digest host 共有の 2 層モデル**: migration 0161 で
  `capsule_static_web_revision_associations` を追加。
  `capsule_static_web_deployments` = 共有 host projection、association = per
  (capsule, revision, materialization) 所有。resolver/activation/takedown/cron を association ベース化。
  同一 bytes の別 Revision・別 Capsule は同一 host を共有。takedown は最後の eligible
  association を失った host のみ suspend。
- unproven secret classification は `secret_classification_unknown` (409) で fail-closed（v1）
- published retry は検証済み `capsuleRevisionId` を ensure（current でなければ何も作らない）

## 3. テストステータス（最終検証済み）

| 対象 | コマンド | 結果 |
| --- | --- | --- |
| Rust (capsule/snapshot/snapshot-builder) | `cargo test -p capsule -p snapshot -p snapshot-builder` + clippy -D warnings | 全パス・clippy クリーン |
| ato-api static-web-deployment | `npx vitest run src/tests/static-web-deployment.test.ts` | 23 パス（host共有2件含む） |
| ato-api static-web-registry | `npx vitest run src/tests/static-web-registry.test.ts` | 24 パス |
| ato-api wizard-confirm-publish | `npx vitest run src/tests/wizard-confirm-publish.test.ts` | 17 パス |
| ato-api wizard 系 | submission-wizard-build 52 / routes 53* / schema 29 / wire 85 / builder-lane 67 | パス |
| ato-api preview/launch 系 | preview-anon-launch 39 / run-preview-contract 26 / launch-preparation 7 ほか | パス |
| ato-edge | `npm test` | 19/19 |
| ato-pwa | `npm test`（変更なし） | 1244/1244 |

\* `submission-wizard-routes` の6件 red は **origin/main 自体でも同一失敗**（worktree で実証済み）＝ baseline。回帰なし。

注意: vitest は**複数ファイル同時実行が workerd 起因で不安定**。必ず1ファイルずつ。

## 4. 主要ファイル

### apps/ato（Rust）
- `crates/snapshot-builder/src/static_web_transport.rs` — agent_id/worker_claim_id/lease header、
  `parse_prepare_response`（identity/generation 検証）、`validate_blob_response_digests`、full-batch verify
- `crates/snapshot-builder/src/static_web_bundle.rs` / `static_web_output.rs` / `static_web_emit.rs` — bundle / `%` 拒否 / 書き出し
- `crates/snapshot-builder/src/authoring_runtime.rs` / `main.rs` — capability 広告、clean_replay 呼び出し
- `crates/capsule/src/foundation/types/manifest_v1.rs` — `%` 拒否（プロデューサ/API パリティ）

### apps/ato-api（Cloudflare Workers）
- `drizzle/0158_static_web_materializations.sql` / `0159_static_web_materialization_closure.sql` /
  `0160_static_web_materialization_closure_digest.sql` / `0161_static_web_revision_associations.sql`
- `src/services/static_web/registry.ts` — prepare/upload-auth/verify/complete + closure + 世代 + lease fencing
- `src/services/static_web/deployment.ts` — 2層モデル（host projection + association）、eligibility 一元化、
  activation CAS、suspend CAS、cron 再評価、KV repair
- `src/services/static_web/resolver.ts` — association 経由解決
- `src/services/static_web/reconcile.ts` — representative materialization 経由活性化
- `src/routes/static_web_artifacts.ts` — lease header + worker_claim_id + generation
- `src/services/submission_wizard/confirm_publish.ts` — main の studio_version CAS 上の secret 分類/拒否/ensure
- `src/services/submission_wizard/authoring_sessions.ts` — seal identity fix

### apps/ato-edge
- `src/manifest.ts` — `STATIC_WEB_FRAME_ANCESTORS_V1` 固定4オリジン + connect_src ソート/ユニーク

## 5. 設計上の重要な決定（変更時は要理解）

- **スナップショットと並存**: static web は既存 Snapshot パイプラインの別レーン（producer contract `ato.static-web-producer/v1`）
- **2層 host モデル（round 8）**: `p-*`/`s-*` host は manifest digest 由来で共有。所有は
  `capsule_static_web_revision_associations`（UNIQUE (capsule_revision_id, environment)）。
  KV は host projection ごと1 record。takedown は最後の eligible association を失った host のみ suspend
- **main の publish CAS**: Capsule の studio_version CAS が publish 全体の唯一 commit point。
  `secrets_required` はその単一 statement に統合。`publish_state_incomplete` / CAS 競合では deployment を作らない
- **secret 分類**: Program Intent のトップレベル bindings（`normalizedProgramIntentEnvelopeV1Schema.shape.intent`）+
  TOML config。v1 で null（unproven）→ `secret_classification_unknown` 拒否
- **CSP は edge で強制**: 4オリジン固定集合・この順序
- **migration はファイル名管理**: 0156/0157 は main の capsule-import-v3（PR #475）が使用。
  static-web は 0158〜0161。rebase 後も番号衝突なしを確認すること

## 6. ブランチ運用の注意

- ルート `ato-run/` は git リポジトリでない。必ず対象アプリのディレクトリで git 操作
- ato / ato-api / ato-edge はローカル feature ブランチのまま
- **ato-api にユーザーWIP `src/routes/capsules.ts` が未コミット**。触らず commit へ混ぜない。
  rebase 時は `git stash push -- src/routes/capsules.ts` → rebase → pop（stash 名 user-wip の慣習）
- ato は PR #1227（draft）。ato-api / ato-edge / ato-pwa は PR 未作成
- コミットは author `koh0920`、Co-Authored-By なし
- ato-edge の push 先は `github.com:ato-run/ato-contents.git`（リポジトリ名が ato-edge でない）

## 7. 既知のハマりどころ

- vitest 複数ファイル同時実行は不安定 → 必ずファイル単位
- `capsule_build_config_revisions` はイミュータブルトリガー付き（UPDATE 不可）
- ato-api rebase は `git rebase -X theirs origin/main` が安全（confirm_publish は最後に static-web 統合を再適用）
- ato-edge は ato-usercontent-static の worktree（git-dir が別）
- migration 適用前は staging D1 の `SELECT COUNT(*) FROM capsule_static_web_materializations;` を確認
  （0件なら現行 migration 適用可、1件以上なら R2 receipt / closure rows からの backfill が必要）

## 8. Next Steps（次のセッションの TODO）

1. **ato-api を最新 main へ rebase**（capsule-import-v3 / PR #475、migration 0156/0157 追加）。
   `-X theirs` で rebase → confirm_publish の static-web 統合（secret分類/拒否/ensure）が残るか確認 →
   migration 0158〜0161 の衝突なしを確認 → 関連 suite をファイル単位で実行
2. **Hextris real-builder E2E**: Docker ホスト or Firecracker builder で snapshot-builder の
   static-web レーンを起動。staging D1/R2 で prepare→upload→verify→complete→
   `capsule_static_web_materializations` に ready。`activateDeployment` で KV/D1 active。
   ato-edge が `s-*`/`p-*` 配信 + CSP 固定集合。ato-pwa で `static_ready` + iframe 直接配信。
   runner/lease/polling/Stop なしを DB・API・ブラウザで確認
3. **PR 化判断**: ato PR #1227 の draft 解除 or 再依頼。ato-api / ato-edge / ato-pwa の PR 作成
4. マージ後: ato.run / app.ato.run で表示確認

## 9. 参考

- 仕様: `apps/ato/docs/rfcs/accepted/`（CAPSULE_SPEC / NACELLE_SPEC / SNAPSHOT 系）、ato-api は `apps/ato-api/docs/rfcs/`
- 本スライスの全ラウンドレビュー指摘と対処はこの handoff の §2 に集約（詳細は各ラウンドの報告）

---

Last updated: 2026-08-05
