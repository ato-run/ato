# UARC V1.1.0 スコープレビュー - 要確認項目

このドキュメントは、UARC V1.1.0仕様との照合で「要確認」となった機能をリストアップしています。

---

## 1. 既にアーカイブ済み（スコープ外確定）

### ✅ `runtime/native.rs`
**理由**: UARC仕様に明記されている

> **UARC SPEC.md 原文**:  
> *"'Native' runtime (direct process execution) is NOT part of UARC V1 due to security and isolation concerns."*

**現状**: アーカイブ済み `.archives/src_legacy/runtime/native.rs`

**問題**: 多数の箇所でNativeRuntimeが参照されているため、コードクリーンアップが必要
- `engine/manager.rs`
- `runtime/mod.rs` (RuntimeKind::Native)
- `runtime/resolver.rs` (Python/Node用にNativeを選択)
- `runtime/container.rs`
- `main.rs`
- `interface/grpc.rs`
- `interface/dev_server.rs`
- `workload/runplan.rs`

**対応案**:
1. **短期**: NativeRuntimeを暫定的に`#[deprecated]`マークし、Source Runtimeへのマイグレーションパスを提供
2. **中期**: RuntimeKind::Nativeを削除し、Source RuntimeへのFallback実装
3. **長期**: 全てのNative参照をSource/OCI/Wasmのいずれかに書き換え

---

### ✅ `system/network/tailscale.rs`
**理由**: インフラレベルのVPNネットワーキング（headscaleと同じ理由）

**現状**: アーカイブ済み `.archives/src_legacy/system/network/tailscale.rs`

**問題**: `main.rs`で`TailscaleManager::start()`が呼ばれている

**対応**: `main.rs`からTailscaleManager初期化コードを削除

---

### ✅ `system/network/traefik.rs`
**理由**: Reverse proxy/ルーティングはCoordinatorスコープ（Edge Router）

**現状**: アーカイブ済み `.archives/src_legacy/system/network/traefik.rs`

**問題**: `engine/manager.rs`で`TraefikManager`がオプション引数として定義されている

**対応**: `CapsuleManager`から`traefik_manager`フィールドとTraefik関連ロジックを削除

---

## 2. 要判断項目

### ⚠️ `resource/model_fetcher.rs`

**内容**: ML model fetching (Hugging Face等からモデルをダウンロード)

```rust
pub struct ModelFetchRequest {
    pub model_id: String,
    pub url: String,
    pub expected_sha256: Option<String>,
}
```

**UARC仕様との関連**:
- ✅ CASベースのダウンロード（`expected_sha256`でverification）
- ✅ L1 Source Policy適用（`validate_path`でパスチェック）
- ❓ ML model固有の処理は一般的な`resource/downloader.rs`と重複？

**論点**:
1. **維持派**: MLモデルは大容量でキャッシュ戦略が異なるため、専用モジュールが有用
2. **統合派**: `downloader.rs`に統合し、model-specificなロジックは上位レイヤー（Coordinator）に移動

**推奨判断軸**:
- Capsuleマニフェストで`model_id`を指定する仕様があるか？
- なければCoordinatorスコープ → **アーカイブ**
- あればEngine実装の一部 → **維持**

---

### ⚠️ `system/network/mdns.rs`

**内容**: mDNS service discovery (`_http._tcp.local.`)

```rust
pub struct MdnsAnnouncer {
    daemon: ServiceDaemon,
    registered_services: Arc<Mutex<HashMap<String, ServiceInfo>>>,
}
```

**UARC仕様との関連**:
- ❓ mDNSはローカルネットワークでのサービス発見（`.local`ドメイン）
- UARC Network Contractは**SPIFFE ID + Egress Policy**を定義
- mDNSはCapsule間通信ではなく、**Engine↔Desktop App**間の発見に使用？

**論点**:
1. **維持派**: DevServer/Desktop統合で必要（開発体験向上）
2. **アーカイブ派**: UARC仕様外のインフラ機能、Service Registryで十分

**推奨判断軸**:
- Desktop Appが`capsuled`を`.local`で発見する必要があるか？
- 必要ならDev専用機能として**維持** (条件付き有効化)
- 不要なら**アーカイブ**

---

### ⚠️ `verification/vram_scrubber.rs`

**内容**: GPU VRAM zeroing for security (secrets残留防止)

```rust
pub trait VramBackend: Send + Sync {
    fn gpu_index(&self) -> usize;
    fn total_bytes(&self) -> Result<u64>;
    fn free_bytes(&self) -> Result<u64>;
    fn zero_chunk(&self, bytes: usize) -> Result<()>;
}
```

**UARC仕様との関連**:
- ✅ セキュリティ関連（L5 Observability? またはL3 Safety gates?）
- ❓ UARC V1.1.0にGPU固有のセキュリティ要件は記載されていない
- ❓ これは「State scrubbing」というより「Hardware isolation」の一種

**論点**:
1. **維持派**: GPU使用後のVRAM残留データは実際のセキュリティリスク（特にマルチテナント環境）
2. **アーカイブ派**: UARC仕様に記載なし、OS/Driver層の責務

**推奨判断軸**:
- GPU対応がUARCの目標スコープに含まれるか？
- 含まれるなら**維持**（将来のUARC v1.2でGPU Isolationとして定義）
- 含まれないなら**アーカイブ**（特殊環境向け拡張）

---

### ⚠️ `bin/deploy_tool.rs`

**内容**: CLI tool for deploying capsule via gRPC

```rust
use capsuled::proto::onescluster::engine::v1::engine_client::EngineClient;
```

**UARC仕様との関連**:
- ❌ `onescluster.engine.v1` - これは旧"Ones Cluster"プロトコル
- UARC V1.1.0は`uarc:v1/`名前空間を定義
- デプロイツールはCoordinator責務（EngineはRuntime実行のみ）

**論点**:
1. **削除派**: Coordinator経由でデプロイすべき、Engine直接デプロイはアンチパターン
2. **移行派**: `onescluster` → `uarc` プロトコルへの移行ツールとして一時的に維持

**推奨判断軸**:
- `onescluster` プロトコルを完全に廃止するか？
- 廃止するなら**削除**
- 移行期間を設けるなら**一時的に維持**（`#[deprecated]`マーク）

---

## 3. 推奨アクション

### Immediate (Phase 11)
1. ✅ `tailscale.rs`, `traefik.rs` の参照削除 (`main.rs`, `engine/manager.rs`)
2. ⚠️ `native.rs` のdeprecation warning追加（削除は破壊的変更のため慎重に）

### Short-term (Phase 12)
3. ⚠️ `model_fetcher.rs` - **判断が必要**
4. ⚠️ `mdns.rs` - **判断が必要** (Dev専用機能として残すか？)
5. ⚠️ `vram_scrubber.rs` - **判断が必要** (GPU対応をスコープに含めるか？)

### Medium-term (Phase 13)
6. `deploy_tool.rs` - `onescluster` → `uarc` プロトコル移行完了後に削除
7. `RuntimeKind::Native` 完全削除、Source Runtimeへの統合

---

## 4. 判断基準の提案

| 機能 | UARC仕様記載 | 実装必須 | 推奨判断 |
|------|-------------|---------|---------|
| **model_fetcher** | ❌ (CAS一般論のみ) | ❌ | Coordinatorへ移動 → **アーカイブ** |
| **mdns** | ❌ | ❌ | Dev専用として**条件付き維持** |
| **vram_scrubber** | ❌ | ❌ | GPU対応をv1.2で定義 → **暫定維持** |
| **deploy_tool** | ❌ (onescluster) | ❌ | プロトコル移行後**削除** |

---

## 次のステップ

1. **このドキュメントをレビュー**して、各項目の判断を決定
2. 決定した項目からPhase 11としてクリーンアップ実施
3. 判断保留の項目はIssue化して、UARC v1.2仕様策定時に再検討
