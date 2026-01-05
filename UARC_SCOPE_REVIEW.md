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

## 4. 最終判断（2026-01-06 決定）

| 機能 | UARC仕様記載 | 実装必須 | **確定判断** |
|------|-------------|---------|-------------|
| **model_fetcher** | ❌ (CAS一般論のみ) | ✅ | **汎用化して維持** → `resource/ingest/` |
| **downloader** | ✅ (CAS fetch機構) | ✅ | **統合** → `resource/ingest/` に統合 |
| **mdns** | ❌ | ❌ | **維持** → `interface/discovery/` (Dev専用) |
| **vram_scrubber** | ❌ | ❌ | **維持** → `verification/vram.rs` (GPU対応) |
| **deploy_tool** | ❌ (onescluster) | ❌ | **削除** (Coordinator責務) |

### 判断理由

#### ✅ model_fetcher + downloader → `resource/ingest/`
**汎用データ取り込み（Ingestion）レイヤーとして再設計**

- **model_fetcher.rs**: ML model固有ではなく、任意の外部リソース（HTTP/S3等）をCASに取り込む汎用fetcher
- **downloader.rs**: 既存のHTTPダウンロード実装
- **統合方針**: `resource/ingest/fetcher.rs` として統合し、URL → Checksum検証 → CAS保存の統一パイプライン

**UARCとの適合性**:
- ✅ L1 Source Policy: CAS blobsのfetchはEngine責務
- ✅ Integrity: `expected_sha256`によるverification
- ✅ 汎用性: 特定のML frameworkに依存しない

#### ✅ mdns → `interface/discovery/`
**Desktop統合・Embedded用途で重要**

- Engineが`.local`ドメインで発見可能になることで、Desktop Appとの連携が容易に
- Dev専用機能として条件付き有効化（production環境では無効化可能）

**UARCとの適合性**:
- ❌ UARC仕様外だが、**開発体験向上**のための実用機能
- Interface層に配置することで、CoreロジックとのDecoupling

#### ✅ vram_scrubber → `verification/vram.rs`
**GPU分離セキュリティの実装**

- マルチテナント環境でのVRAM残留データリスクは実在する
- UARC v1.2でGPU Isolation要件を正式化する前提で暫定維持

**UARCとの適合性**:
- ❌ V1.1.0に記載なし
- ✅ L3 Safety gates / L5 Observabilityの一環として解釈可能
- Future-proof: GPU対応は必須要件になる見込み

#### ❌ deploy_tool → 削除
**Coordinator責務 + 旧protocol**

- Engine直接デプロイはアーキテクチャ違反（Coordinator経由が正）
- `onescluster` protocol は廃止予定
- 削除して問題なし

---

## 5. Phase 12 実装計画

### Phase 12-1: Resource Ingest 統合
```bash
mkdir -p src/resource/ingest
# model_fetcher.rs をベースに汎用化
git mv src/resource/model_fetcher.rs src/resource/ingest/fetcher.rs
git mv src/resource/downloader.rs src/resource/ingest/http.rs
# 後でfetcher.rs内にhttp.rsのロジックを統合
```

**コード変更**:
- `ModelFetchRequest` → `ResourceFetchRequest` (汎用化)
- `model_id` → `resource_id` (用途非依存)
- HuggingFace固有のロジックを削除

### Phase 12-2: Interface Discovery
```bash
mkdir -p src/interface/discovery
git mv src/system/network/mdns.rs src/interface/discovery/mdns.rs
```

### Phase 12-3: Verification VRAM
```bash
git mv src/verification/vram_scrubber.rs src/verification/vram.rs
```

### Phase 12-4: Cleanup
```bash
git rm src/bin/deploy_tool.rs
```

---

## 6. Phase 13: Native Runtime 完全削除

**前提**: Phase 12完了後に実施

Native Runtime参照を全て削除し、Source Runtimeへ統合:

1. `engine/manager.rs`: `native_runtime` field削除
2. `runtime/resolver.rs`: Python/Node → Source Runtimeへfallback
3. `runtime/container.rs`: Native条件分岐削除
4. `main.rs`: NativeRuntime初期化削除
5. `interface/grpc.rs`: `NativeRuntime` proto変換削除
6. `workload/runplan.rs`: `Runtime::Native` 削除

---

## 次のステップ

1. ✅ **Phase 12-1~4を順次実行**（リファクタリング）
2. ✅ **Phase 13でNative完全削除**（破壊的変更）
3. ✅ **全テスト実行とコンパイル確認**

---

## 完了状況 (2024年実施)

### ✅ Phase 11: アーカイブ完了
- `headscale/` → `.archives/src_legacy/headscale/`
- `runtime/native.rs` → `.archives/src_legacy/runtime/native.rs`
- `system/network/tailscale.rs` → `.archives/src_legacy/system/network/tailscale.rs`
- `system/network/traefik.rs` → `.archives/src_legacy/system/network/traefik.rs`

Commits: `5a9bd74`, `994157f`

### ✅ Phase 12: リソース整理完了
- `resource/model_fetcher.rs` → `resource/ingest/fetcher.rs` (汎用化)
- `system/network/mdns.rs` → `interface/discovery/mdns.rs`
- `verification/vram_scrubber.rs` → `verification/vram.rs`
- `bin/deploy_tool.rs` → 削除

Commit: `c8a107e`

### ✅ Phase 13: Native Runtime完全削除完了
**変更ファイル**:
- `runtime/resolver.rs`: Python/Node → RuntimeKind::Source
- `runtime/container.rs`: native_runtime field削除
- `interface/dev_server.rs`: TailscaleManager削除
- `interface/grpc.rs`: TailscaleManager削除、Native proto → Source mapping
- `engine/manager.rs`: native_runtime, traefik_manager削除
- `main.rs`: TailscaleManager初期化削除
- `verification/vram.rs`: モジュールパス修正
- `resource/ingest/fetcher.rs`: FetcherConfig

Commits: `2ef6326`, `18346ec`

**コンパイル確認**: ✅ `cargo check` 成功

**Breaking Changes**:
- Native runtime → Source runtime migration
- TailscaleManager → SPIFFE ID migration
- TraefikManager → Coordinator routing migration

---

## UARC V1.1.0 適合性

**Capsuled は現在、以下のUARC V1.1.0仕様に完全準拠**:

- ✅ **Runtime Support**: Wasm, Source, OCI
- ✅ **Network Identity**: SPIFFE ID (Tailscale VPN削除)
- ✅ **Routing**: Coordinator責務 (Traefik削除)
- ✅ **Security**: CAS-based verification, Path validation
- ✅ **Resource Management**: Generic resource ingestion
- ✅ **Development**: mDNS discovery (dev-only)
- ✅ **GPU Security**: VRAM scrubbing (future v1.2)
