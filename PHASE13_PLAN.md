# Phase 13: Native Runtime 完全削除

## 目標
UARC V1.1.0準拠 - Native runtime完全削除とSource Runtimeへの統合

---

## 背景

UARC仕様明記:
> *"'Native' runtime (direct process execution) is NOT part of UARC V1 due to security and isolation concerns."*

Phase 11でファイルをアーカイブ、Phase 13で全参照を削除します。

---

## 影響範囲

### 削除対象

1. **engine/manager.rs**
   - `native_runtime: Arc<NativeRuntime>` field
   - `NativeRuntime::new()` 初期化
   - `RuntimeKind::Native` 参照
   - `native_runtime.launch()` 呼び出し

2. **runtime/resolver.rs**
   - Python/Node → `RuntimeKind::Native` fallback
   - → `RuntimeKind::Source` へ変更

3. **runtime/container.rs**
   - `native_runtime: Option<Arc<NativeRuntime>>` field
   - Native条件分岐

4. **main.rs**
   - `RuntimeKind::Native` fallback設定
   - NativeRuntime関連ロジック

5. **interface/grpc.rs**
   - `common::NativeRuntime` proto変換
   - `TailscaleManager` import（アーカイブ済み）

6. **interface/dev_server.rs**
   - `RuntimeKind::Native` デフォルト設定

7. **workload/runplan.rs**
   - `Runtime::Native` proto定義参照

---

## 実施手順

### Step 1: runtime/resolver.rs
Python/NodeをNativeではなくSourceにfallback

```rust
// Before
"python" | "python3" => RuntimeKind::Native,
"node" | "nodejs" | "deno" => RuntimeKind::Native,

// After  
"python" | "python3" => RuntimeKind::Source,
"node" | "nodejs" | "deno" => RuntimeKind::Source,
```

### Step 2: interface/dev_server.rs
デフォルトruntimeをNativeからSourceへ

```rust
// Before
kind: RuntimeKind::Native,

// After
kind: RuntimeKind::Source,
```

### Step 3: interface/grpc.rs
- NativeRuntime proto変換削除
- TailscaleManager import削除

### Step 4: engine/manager.rs
- `native_runtime` field削除
- 初期化コード削除
- RuntimeKind::Native参照削除
- launch呼び出し削除

### Step 5: runtime/container.rs
- `native_runtime` field削除
- Native条件分岐削除

### Step 6: main.rs
- RuntimeKind::Native fallback削除
- 関連ロジック削除

### Step 7: workload/runplan.rs
- `Runtime::Native` 参照削除（protobuf定義参照のため要注意）

---

## 注意事項

### Proto定義の扱い
`workload/runplan.rs`が参照している`common::NativeRuntime`はproto定義のため、
以下の対応が必要：

1. Proto定義（.proto）から`NativeRuntime`削除
2. `buf generate`で再生成
3. Rust側の参照を削除

または：

1. Proto定義は残す（後方互換性）
2. Rust側でmatch時に`Runtime::Native`を無視

→ **推奨**: Proto定義は残し、Rust側でwarning抑制

---

## 実行

全ファイルを一括修正後、コンパイル確認します。
