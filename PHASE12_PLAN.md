# Phase 12: Resource Ingest統合 + Discovery/VRAM移動

## 目標
UARC V1.1.0準拠の最終的な構造へ移行する

---

## Phase 12-1: Resource Ingest 統合

### 目的
`model_fetcher.rs` と `downloader.rs` を汎用データ取り込みレイヤーとして統合

### 実施手順

```bash
# 1. 新ディレクトリ作成
mkdir -p src/resource/ingest

# 2. ファイル移動
git mv src/resource/model_fetcher.rs src/resource/ingest/fetcher.rs
git mv src/resource/downloader.rs src/resource/ingest/http.rs
```

### コード変更（fetcher.rs）

**Before:**
```rust
pub struct ModelFetchRequest {
    pub model_id: String,
    pub url: String,
    pub expected_sha256: Option<String>,
}
```

**After:**
```rust
pub struct ResourceFetchRequest {
    pub resource_id: String,  // 汎用ID（model_id, artifact_id等）
    pub url: String,
    pub expected_sha256: Option<String>,
}

pub async fn fetch_resource(
    req: ResourceFetchRequest,
    cfg: FetcherConfig,
) -> Result<FetchResult>
```

### 更新が必要なファイル
- `src/resource/mod.rs`: `pub mod ingest;` 追加
- `src/resource/ingest/mod.rs`: 新規作成
- 参照元を検索して `model_fetcher` → `ingest::fetcher` に置換

---

## Phase 12-2: Interface Discovery

### 目的
mDNSをInterface層のDiscovery機能として配置

### 実施手順

```bash
# 1. 新ディレクトリ作成
mkdir -p src/interface/discovery

# 2. ファイル移動
git mv src/system/network/mdns.rs src/interface/discovery/mdns.rs

# 3. mod.rs作成
cat > src/interface/discovery/mod.rs << 'EOF'
//! Service Discovery for Embedded/Desktop integration
//!
//! mDNS announcer for .local domain advertisement

pub mod mdns;

pub use mdns::MdnsAnnouncer;
EOF
```

### 更新が必要なファイル
- `src/interface/mod.rs`: `pub mod discovery;` 追加
- `src/system/network/mod.rs`: `pub mod mdns;` 削除
- `src/lib.rs`: backward compatibility re-export更新

---

## Phase 12-3: Verification VRAM

### 目的
VRAM Scrubberを検証レイヤーに配置

### 実施手順

```bash
# ファイル移動
git mv src/verification/vram_scrubber.rs src/verification/vram.rs
```

### 更新が必要なファイル
- `src/verification/mod.rs`: `pub mod vram_scrubber;` → `pub mod vram;`
- `src/lib.rs`: `pub use verification::vram_scrubber as vram_scrubber;` 削除（破壊的変更）
- 参照元を検索して `vram_scrubber` → `vram` に置換

---

## Phase 12-4: Deploy Tool 削除

### 目的
旧onescluster protocolのdeploy toolを削除

### 実施手順

```bash
# ファイル削除
git rm src/bin/deploy_tool.rs
```

### 確認事項
- `Cargo.toml` の `[[bin]]` セクションに `deploy_tool` があれば削除
- `README.md` に deploy_tool の説明があれば削除

---

## Phase 12-5: system/network クリーンアップ

### 目的
mdns移動後、system/networkに残るのは `service_registry.rs` のみ

### 実施手順

```bash
# mdns.rs移動済みを確認
ls -la src/system/network/

# service_registry.rs のみなら、networkディレクトリを削除して直下に配置
git mv src/system/network/service_registry.rs src/system/service_registry.rs
rmdir src/system/network

# または network/ を維持する場合は何もしない
```

### 更新が必要なファイル
- `src/system/mod.rs`: `pub mod network;` → 削除 or 維持
- `src/system/mod.rs`: `pub mod service_registry;` 追加（直下に移動した場合）

---

## Phase 12 完了後の構造

```
src/
├── resource/
│   ├── ingest/          # ✨ NEW
│   │   ├── fetcher.rs   # 汎用リソース取得
│   │   ├── http.rs      # HTTP実装
│   │   └── mod.rs
│   ├── artifact/
│   ├── cas/
│   ├── storage/
│   ├── oci/
│   └── mod.rs
│
├── interface/
│   ├── discovery/       # ✨ NEW
│   │   ├── mdns.rs      # (from system/network/)
│   │   └── mod.rs
│   ├── grpc.rs
│   ├── http.rs
│   ├── api.rs
│   ├── dev_server.rs
│   └── mod.rs
│
├── verification/
│   ├── vram.rs          # ✨ RENAMED (from vram_scrubber.rs)
│   ├── verifier.rs
│   ├── signing.rs
│   ├── egress_policy.rs
│   ├── egress_proxy.rs
│   ├── path.rs
│   ├── dns_monitor.rs
│   └── mod.rs
│
├── system/
│   ├── hardware/
│   ├── service_registry.rs  # (network/削除想定)
│   └── mod.rs
│
└── bin/
    # ❌ deploy_tool.rs 削除済み
```

---

## 実行順序

1. ✅ Phase 12-1: Resource Ingest統合
2. ✅ Phase 12-2: Interface Discovery
3. ✅ Phase 12-3: Verification VRAM
4. ✅ Phase 12-4: Deploy Tool削除
5. ✅ Phase 12-5: system/network クリーンアップ
6. ✅ コンパイル確認 (`cargo check`)
7. ✅ テスト実行 (`cargo test`)
8. ✅ コミット

---

## 次のフェーズ

**Phase 13**: Native Runtime完全削除
- 参照元の全削除（破壊的変更）
- Source Runtimeへの統合
- 詳細は `PHASE13_PLAN.md` を参照
