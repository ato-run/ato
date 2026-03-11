# .capsule バージョン管理とロールバック実装レポート（現状 + 追加実装）

作成日: 2026-03-05

## 0. 目的と前提
- 目的: `.capsule` のバージョン管理の「現行実装」と、ロールバックを安全運用するための「追加実装」を整理する。
- 根拠: 本リポジトリの実コード（`core/`, `src/`）を一次情報として確認した。
- 注記: 本レポートは、提示された PR1〜PR4 の論点（Atomic/Manifest+CAS/GC/Protocol+AuthZ）にマッピングして記述する。

---

## 1. エグゼクティブサマリ
- 現在の v3 実装は、`CapsuleManifestV3`（JCS + BLAKE3）と `epoch` ポインタを中心に動いている。
- ロールバックは「過去 manifest への巻き戻し」ではなく、「過去 manifest を指す新しい epoch を前進追加」する方式で実装済み。
- `negotiate` は Bloom フィルタ / exact have-chunks / lease 再利用に対応し、差分チャンク取得は実装済み。
- GC は `tombstone + gc_queue + lease + live_reference` で最低限の保護がある。
- ただし、ロールバック安全性の観点では以下が未完了。
  - tombstone 済み manifest へのロールバック時に「復帰（untombstone）」しないため、GC が現行 manifest のチャンクを削除し得る。
  - 認可はグローバル Bearer token ベースで、`scoped_id` 単位の AuthZ 再評価がない。
  - ダウングレード対策は「epoch 後退検知」中心で、`古い脆弱 manifest を新しい epoch で正当に配る` ケースを抑止できない。

---

## 2. 現在の実装（コード事実）

### 2-1. v3 マニフェストと決定論的ハッシュ
- `CapsuleManifestV3` は `schema_version/manifest_hash/merkle_root/chunk_list/signatures` を持つ。
  - `core/src/types/capsule_v3.rs`
- payload は FastCDC で分割し、各チャンクを `blake3:` でアドレス化。
  - `core/src/resource/cas/chunker.rs`
- `manifest_hash` は `manifest_hash` と `signatures` を除いた JCS 正規化 JSON の BLAKE3。
  - `core/src/packers/payload_v3.rs`
  - `src/registry_v3.rs`

### 2-2. `.capsule` 生成と v3 必須化
- pack 時に `manifest.v3.jcs` + `payload.tar.zst` を生成して `.capsule` に格納。
  - `core/src/packers/capsule.rs`
- local registry upload 時に `manifest.v3.jcs` / `payload.tar.zst` が必須。
  - `src/registry_serve.rs`（`handle_put_local_capsule`）

### 2-3. バージョン管理の実体（epoch ポインタ）
- `capsules.current_epoch` が「現在のバージョン」を示し、`epochs(scoped_id, epoch, manifest_hash, prev_epoch_hash, signature...)` が履歴を持つ。
- つまり v3 のバージョン管理は、manifest DAG のうち `current_epoch` の指すルート切替として表現されている。
  - `src/registry_v3.rs`（schema と `record_manifest_and_epoch`）

### 2-4. ロールバック実装
- API: `POST /v1/v3/rollback`
  - `src/registry_serve.rs`
- ストア実装: `rollback_to_manifest(scoped_id, target_manifest_hash)`
  - 対象 manifest が履歴に存在することを確認
  - `current_epoch + 1` の新規 epoch を作り、`manifest_hash=target` を指す
  - `capsules.current_epoch` を同トランザクションで更新
  - `journal` に開始/完了を記録
  - `src/registry_v3.rs`
- CLI: `ato rollback --to-manifest blake3:...`
  - `src/main.rs`

### 2-5. 差分同期（negotiate）
- API: `POST /v1/v3/negotiate`
- `have_chunks` または Bloom (`have_chunks_bloom`) を受け、不足チャンクのみ返す。
- `reuse_lease_id` によりリトライ時の lease 再利用に対応。
- クライアントは `LocalCasIndex` から Bloom を作り、偽陽性時は exact have-chunks で再 negotiate。
  - `src/registry_v3.rs`
  - `src/install.rs`
  - `core/src/resource/cas/bloom.rs`, `core/src/resource/cas/index.rs`

### 2-6. GC と可用性保護
- delete 系操作で manifest/chunk を tombstone し、`gc_queue` へ enqueue。
- GC worker が `gc_tick` を定期実行。
- 削除前に以下を確認して defer する。
  - chunk tombstone の有無
  - active lease の有無
  - tombstone されていない manifest からの live reference の有無
- `leases` は chunk 単位の複合 PK (`lease_id, chunk_hash`)。
  - `src/registry_v3.rs`
  - `src/registry_serve.rs`（GC worker / delete tombstone）

### 2-7. セキュリティ実装（現状）
- epoch pointer は Ed25519 署名付きで配布され、install 側で DID 整合と署名検証を実施。
  - `src/install.rs`（`verify_epoch_signature`）
- install 側は `epoch-guard.json` で最大 epoch を記録し、後退 epoch を拒否（`--allow-downgrade` で例外）。
  - `src/install.rs`（`enforce_epoch_monotonicity`）
- read/write API は Bearer token を検証。
  - `src/registry_serve.rs`（`validate_read_auth`, `validate_write_auth`）

---

## 3. PR1〜PR4 対応の進捗評価

### PR1（Atomic Switch）
- 実装済み
  - rollback の epoch 切替は DB トランザクション内で原子的。
  - epoch guard state は `tmp -> rename` で原子的更新。
  - local CAS chunk 書き込みも `tmp -> rename`。
- 未完了/要改善
  - install 先 `.capsule` の保存は直接 `File::create + write_all` で原子的ではない。

### PR2（Manifest/CAS ベースのバージョン管理）
- 実装済み
  - `CapsuleManifestV3`、JCS、BLAKE3、FastCDC、Merkle root。
  - registry の `manifests/chunks/manifest_chunks/epochs/capsules` モデル。
- 未完了/要改善
  - manifest `signatures` フィールドの信頼チェーン検証は実運用フローに未統合（epoch 署名中心）。

### PR3（GC/参照保護）
- 実装済み
  - `tombstone + gc_queue + lease + live_reference` で削除保護。
  - インデックス・マイグレーション・定期 worker あり。
- 未完了/要改善
  - tombstone manifest を rollback で current に戻した時の「untombstone/pin 復帰」がない。
  - 中長期保持ポリシー（rollback 可能期間の明示）や復元保証 SLA が未定義。

### PR4（Protocol + AuthZ）
- 実装済み
  - `/v1/v3/negotiate`, `/v1/v3/epoch/resolve`, `/v1/v3/rollback`, `/v1/v3/leases/*`。
  - negotiate は履歴外 manifest を拒否。
- 未完了/要改善
  - AuthZ はグローバルトークン中心で、`scoped_id` 単位・バージョン単位の権限モデルがない。

---

## 4. 3人格レビュー（提示論点との整合）

### 【人格1: CS教授（構造・理論）】
- 評価: 提示どおり、現行 v3 は「不変 chunk 群 + manifest hash + epoch pointer」によるルート切替モデルに近い。
- 補足: 正確には「DAG の mutable root は `capsules.current_epoch`」で、rollback も forward epoch 追加で表現される。
- 課題: rollback の可用性は retention/GC 設計に依存し、ここがまだ制度化不足。

### 【人格3: フルスタック（Rust/性能）】
- 評価: 「メタデータ再リンクで高速」は概ね正しい。rollback 本体は DB 更新中心で軽い。
- 評価: negotiate 差分同期 + Bloom + lease 再利用は実装済みで、チャンク再利用パスは実戦投入可能。
- 課題: tombstone 済み履歴に戻すケースで materialization が保証されず、実行時に欠損し得る。

### 【人格2: SWE-S（セキュリティ）】
- 評価: ダウングレード攻撃面の懸念は有効。
- 現状の防御: epoch 後退検知（client 側）と epoch 署名検証。
- 盲点: 攻撃者が正当 write 権限を得た場合、古い脆弱 manifest を「新しい epoch」で配布でき、epoch 後退検知を回避可能。
- 課題: rollback の policy gate（許可条件/承認/最小セキュリティ基準）が必要。

---

## 5. ロールバック安全化のための追加実装（必須）

1. rollback 時の manifest 復帰処理
- `rollback_to_manifest` 実行時に target manifest と依存 chunks を `untombstone` する。
- 既存 `gc_queue` に載っている対象 chunk を cancel/deferred へ遷移させる。
- これをしないと、current epoch が参照していても GC に消され得る。

2. rollback 可否チェック（materialization 保証）
- rollback 前に `manifest_chunks` と chunk 実体の完全性を検証。
- 欠損がある場合は rollback を拒否し、`negotiate`/復旧手順を先に要求。

3. AuthZ の細粒度化
- `scoped_id` ごとの read/write/rollback 権限を導入。
- `negotiate` / `epoch resolve` / `rollback` で同じ AuthZ ルールを再評価。
- 監査ログ（誰がいつどの manifest に rollback したか）を強化。

4. rollback policy（ダウングレード耐性）
- 例: `minimum_safe_epoch` または `blocked_manifest_hashes` を policy として評価。
- 高危険 rollback は二段階承認（MFA/別権限）にする。

5. 保持ポリシーの明文化
- 「何世代・何日 rollback 可能か」を policy 化し、GC と整合させる。
- rollback 対象の retention pin（短期/中期）を追加。

6. クライアント側厳格化（任意だが推奨）
- epoch だけでなく、manifest 単位の許可/拒否ポリシーを適用。
- install 保存を `tmp -> rename` 化して中断耐性を上げる。

---

## 6. 優先度付き実装順
1. P0: rollback 時の `untombstone + gc_queue 調整 + 完全性チェック`
2. P0: `scoped_id` 単位 AuthZ と rollback 監査強化
3. P1: rollback policy（最小安全基準、blocked manifest）
4. P1: retention pin と GC policy の制度化
5. P2: install 保存の原子的更新、運用 UX 改善

---

## 7. 結論
- Ato v3 の基盤（JCS manifest / BLAKE3 / FastCDC / epoch / negotiate / lease / GC）は、ロールバック可能な設計として既に成立している。
- ただし「安全な rollback 運用」を成立させる最後の壁は、GC と rollback の整合、細粒度 AuthZ、policy ベースの downgrade 抑止である。
- 最優先は、`rollback で current に戻した manifest が GC で再削除されない` ことをコード上で保証する実装である。
