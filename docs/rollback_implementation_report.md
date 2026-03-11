# `.capsule` バージョン管理 / ロールバック実装調査レポート

## 結論

現在の実装は、**レジストリ側の rollback と CAS ベースの差分再構築**についてはかなり入っています。  
ただし、依頼文にあるロールバック要件を全部満たしているわけではありません。

- **実装済み**
  - JCS + BLAKE3 による決定論的 manifest hash
  - FastCDC による chunk 化
  - ローカル CAS を使った payload 再構築
  - `negotiate` + chunk lease による不足 chunk だけの取得
  - レジストリ側 rollback API
  - yanked manifest の fail closed
  - epoch 署名とクライアント側の downgrade guard
- **部分実装**
  - GC 安全性
  - アトミック切り替え
  - バージョン履歴の保持保証
- **未実装 / 要件未達**
  - `ato run` で過去 `manifest_hash` を直接指定してローカル rollback する経路
  - 過去バージョンに対するユーザー単位の AuthZ 再検証
  - GC から過去バージョンを恒久的に守る静的 refcount 設計

---

## 1. バージョン管理の構造的定義

### 判定: **概ね実装済み**

### 根拠

- manifest は JCS で canonicalize され、BLAKE3 で `manifest_hash` を計算しています。  
  `core/src/packers/payload.rs:86-166`
- payload は FastCDC で deterministic に chunk 化され、各 chunk は `blake3:` digest を持ちます。  
  `core/src/resource/cas/chunker.rs:4-29`
- manifest には `distribution.manifest_hash`, `merkle_root`, `chunk_list` が入ります。  
  `core/src/types/manifest.rs:434-452`

### 重要な差分

依頼文は `CapsuleManifestV3` を前提にしていますが、現行コード上はその型名は見当たりません。  
実装は `CapsuleManifest` に `distribution` を持たせる形です。

- manifest 生成時に `schema_version = "1"` を入れています。  
  `core/src/packers/payload.rs:102-115`

つまり、**思想は一致**していますが、**コード上のモデル名と公開表現は依頼文の説明そのままではありません**。

---

## 2. ローカルロールバック

### 判定: **部分実装**

### できること

`install` 系の delta path は、現在の epoch pointer が指す manifest について、

1. manifest を取得  
2. ローカル CAS の bloom を送って `negotiate`  
3. 不足 chunk だけ取得  
4. ローカル chunk から payload を再構築

という流れを持っています。

- current epoch を解決して manifest を取得  
  `src/install.rs:526-602`
- CAS bloom を送って negotiate  
  `src/install.rs:604-621`
- ローカル chunk だけで payload を復元  
  `src/install.rs:636-696`, `src/install.rs:863-891`
- ローカル CAS は `~/.ato/cas` 既定で保存されます  
  `core/src/resource/cas/index.rs:25-33`

### できないこと

依頼文にある

> `ato run` で過去の `manifest_hash` を指定すれば即座に復元できる

という経路は、**現状の CLI にはありません**。

- `Rollback` コマンドはあるが、これはローカル実行ではなく **レジストリ API 呼び出し**です。  
  `src/main.rs:300-323`, `src/main.rs:3716-3779`
- `ato run` は `publisher/slug` かローカル path を受け取り、インストール済みアーカイブか最新の auto-install に流れます。  
  `src/main.rs:3163-3279`
- `ato run` 側に `manifest_hash` を直接渡す引数は見当たりません。  
  `src/main.rs:3163-3279`

さらに、`install_app` は `version` の存在確認はしますが、delta install 自体は **常に `epoch/resolve` の current pointer を使います**。

- 指定 version は release 一覧に存在するかだけ確認  
  `src/install.rs:351-371`
- 実際の delta install 呼び出しには version が渡っていない  
  `src/install.rs:372-378`
- delta install 内では `scoped_id` だけで current epoch を引いている  
  `src/install.rs:526-551`

したがって、**「任意の過去 manifest をローカルで選んで即復元」ではなく、「現在 pointer が指す manifest を CAS 差分復元する」実装です**。

---

## 3. レジストリ側 rollback / negotiate

### 判定: **実装済み**

### 根拠

- publish 時に manifest, chunks, manifest_chunks, epochs, current epoch を保存しています。  
  `src/registry_store.rs:428-478`, `src/registry_store.rs:745-912`, `src/registry_store.rs:998-1065`
- rollback は `target_manifest_hash` が履歴に存在することを確認し、chunk 実体も検証したうえで、**epoch を巻き戻すのではなく新しい epoch を前進させて** current pointer を切り替えます。  
  `src/registry_store.rs:1067-1258`
- `negotiate` は対象 manifest の chunk 列を見て、クライアントが未保有の chunk だけ返します。  
  `src/registry_store.rs:1286-1398`

### 補足

rollback は「古い epoch を復活」ではなく、**古い manifest を指す新しい epoch を発行**します。

- `next_epoch = current_epoch + 1`  
  `src/registry_store.rs:1182-1195`

これは downgrade guard と整合しています。  
クライアントは古い epoch replay を拒否しますが、**正規 rollback は forward epoch** なので拒否されません。

---

## 4. GC と可用性

### 判定: **部分実装**

### 実装されていること

- delete 時、release 参照がなくなった manifest は tombstone され、chunk は GC queue に入ります。  
  `src/registry_store.rs:480-566`
- `negotiate` は対象 manifest の全 chunk に lease を張ります。  
  `src/registry_store.rs:1366-1468`
- GC は `active_lease` または `live_reference` があれば削除を deferred にします。  
  `src/registry_store.rs:1608-1718`
- rollback 実行時は target manifest/chunk を untombstone し、GC queue から外します。  
  `src/registry_store.rs:1211-1229`

### 未達な点

依頼文が求める

> 一度発行された `manifest_hash` に紐づくデータを、その hash が有効な限り保持する

は **満たしていません**。

理由は、GC の live 判定が **epoch 履歴そのものではなく `manifests.tombstoned_at IS NULL`** を見ているためです。

- GC の live 判定  
  `src/registry_store.rs:1659-1672`

つまり、

1. 過去 manifest が履歴 `epochs` には残る  
2. しかし delete により tombstone 済み  
3. lease も無い  
4. chunk が GC される  

という流れが普通に起きます。  
実際、rollback 実装も「chunk がもう無ければ失敗する」前提です。

- rollback は target chunk の DB 行と実ファイルを両方確認し、欠損時は fail  
  `src/registry_store.rs:1106-1153`

したがって、**rollback 可用性は GC ポリシー依存**であり、依頼文の説明と一致します。

ただし、保護は **lease + tombstone/live-reference** ベースで、**永続 refcount** ではありません。

---

## 5. セキュリティ / ダウングレード耐性

### 判定: **部分実装**

### 実装されていること

- epoch pointer は署名付きで、クライアントは公開鍵/DID/署名を検証します。  
  `src/registry_store.rs:998-1065`, `src/install.rs:1208-1255`
- クライアントは `~/.ato/state/epoch-guard.json` に最大 epoch を覚え、古い epoch をデフォルト拒否します。  
  `src/install.rs:1413-1544`
- yanked manifest は manifest fetch / negotiate / install を fail closed します。  
  `src/registry_store.rs:1261-1283`, `src/registry_store.rs:1286-1317`, `src/registry_serve.rs:929-1013`, `src/install.rs:563-581`, `src/install.rs:720-740`

### 未達な点

- rollback 権限は **registry 全体の Bearer token** でしか見ていません。  
  `src/registry_serve.rs:1198-1238`, `src/registry_serve.rs:2558-2594`
- read auth も同じく **単一トークン** で、ユーザー単位・capsule 単位・manifest 単位の AuthZ はありません。  
  `src/registry_serve.rs:929-1138`, `src/registry_serve.rs:2558-2594`

よって、依頼文の

> rollback 先の過去バージョンに対しても、現在のユーザーがアクセス権限を持っているかを `negotiate` フェーズで再検証

は **未実装**です。  
現状は「その registry の共有 token を持っているか」しか見ていません。

### 実質的な評価

- **無権限の古い epoch replay** には強い
- **権限者による rollback** は正規操作として通る
- **細粒度 AuthZ** は未整備

です。

---

## 6. Atomic Switch

### 判定: **部分実装**

### 実装されていること

- install された `.capsule` ファイル自体は temp file + `fsync` + `rename` で atomically 書き込みます。  
  `src/install.rs:1018-1055`
- local CAS chunk 書き込みも temp file + `rename` です。  
  `core/src/resource/cas/index.rs:54-68`, `core/src/resource/cas/index.rs:280-297`
- epoch guard state も atomic write です。  
  `src/install.rs:1509-1544`

### 未達な点

依頼文の

> rollback 展開は `tmp` ディレクトリ展開 -> `rename`

という意味での **展開済み runtime tree の atomic switch** は、今回見た範囲では実装されていません。  
現状の atomicity は **artifact file / chunk file / state file** の書き換えに留まります。

---

## 7. 要件別判定表

| 要件 | 判定 | コメント |
|---|---|---|
| JCS + BLAKE3 による決定論的 version | 実装済み | `CapsuleManifestV3` という型名ではない |
| FastCDC による構造的共有 | 実装済み | chunk 単位で CAS 再利用 |
| `manifest_hash` 指定のローカル rollback | 未実装 | `ato run` から直接指定できない |
| negotiate による不足 chunk のみ取得 | 実装済み | bloom + exact retry + lease あり |
| レジストリ rollback API | 実装済み | forward epoch で current を切替 |
| GC に負けない rollback | 部分実装 | lease 中は守れるが永続保証はない |
| rollback 中の GC/TOCTOU 耐性 | 部分実装 | chunk lease はあるが静的 refcount ではない |
| downgrade 攻撃耐性 | 部分実装 | epoch 署名 + local max epoch guard はある |
| rollback 先への AuthZ 再検証 | 未実装 | 共有 Bearer token のみ |
| atomic switch | 部分実装 | artifact/chunk/state は atomic、runtime tree switch は未確認 |

---

## 8. テストで確認したもの

以下の対象テストはローカルで実行して通過しました。

- `cargo test -p ato-cli rollback_`
- `cargo test -p ato-cli test_delta_install`
- `cargo test -p ato-cli test_manifest_yanked`
- `cargo test -p ato-cli test_epoch_guard`
- `cargo test -p ato-cli test_atomic_install_writes_via_tmp_and_rename`

特に以下は今回の論点に直接対応しています。

- `rollback_creates_forward_epoch_transition`
- `rollback_fails_when_chunk_missing`
- `rollback_untombstones_manifest_and_chunks`
- `rollback_clears_gc_queue_for_target_chunks`
- `rollback_rejects_yanked_manifest`
- `test_delta_install_false_positive_recovers_with_reuse_lease_id`
- `test_manifest_yanked_fails_closed_even_with_allow_unverified`
- `test_epoch_guard_rejects_downgrade_without_flag`

---

## 最終評価

依頼文のまとめに対する評価は次のとおりです。

1. **「ロールバックは技術的に可能で軽量」**  
   これは **概ね正しい** です。特にレジストリ rollback と CAS 差分再構築は実装されています。

2. **「ローカルでは過去 manifest_hash を指定すれば `ato run` で即復元できる」**  
   これは **現状の CLI 実装とは一致しません**。`ato run` にその指定経路はありません。

3. **「安全に行うために必要な実装が全て入っている」**  
   これは **未達** です。  
   特に不足しているのは以下です。

- 過去 manifest を直接選ぶローカル実行 UX
- GC から履歴を守る永続 refcount/retention policy
- 過去 version に対する細粒度 AuthZ
- 展開済み runtime tree の atomic rollback switch
