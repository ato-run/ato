---
title: "Phase 13b.9 Guest Protocol JSON-RPC 2.0 移行 — 設計案 (v2.1 改訂)"
date: 2026-04-29
author: "@egamikohsuke (deep-research + survey + review)"
status: proposal-v2.1
related:
  - docs/TODO.md (Phase 13b.9)
  - crates/ato-cli/src/cli/commands/guest.rs (777 行)
  - crates/ato-cli/src/adapters/ipc/guest_protocol.rs (107 行)
  - crates/ato-cli/src/adapters/ipc/jsonrpc.rs (441 行, Phase 13b.8 成果)
  - crates/ato-cli/tests/guest_e2e.rs (418 行)
  - crates/ato-desktop/src/bridge.rs (capsule/invoke を別 shape で送信中)
  - claudedocs/research_phase13a_sandbox_best_practices_20260429.md
revision_notes: |
  v1 → v2: 外部レビューにより 6 ブロッカー + 4 中程度懸念を特定。
  実コード読み直しで env 名・JsonRpcResponse API・InvokeParams shape・
  ato-desktop bridge.rs の capsule/invoke 用法を確認し、計画を是正。
  v2 → v2.1: ユーザ指示に基づく 2 つの方針修正 ——
  (1) env 名を新命名規則 (`CAPSULE_IPC_*`) で統一、旧名は破棄 (fallback なし)
      旧名の書き手は workspace 内ソースに存在しないため安全にリネーム可。
  (2) WASM/OCI ランタイム関連は本 PR 対象外に defer。
      `capsule/wasm.execute` メソッドは導入しない。ExecuteWasm は guest.v1 経路で
      引き続き利用可。OCI/WASM executor の env 注入も本 PR では触らない。
---

# Phase 13b.9 Guest Protocol JSON-RPC 2.0 移行 (v2)

## TL;DR (v2.1)

ato-cli `ato guest` の `guest.v1` 独自プロトコルを JSON-RPC 2.0 に移行する。
LSP 流の **envelope auto-detect** で旧/新を併設し、明示的 negotiation は不要。

v2.1 で確定した点:

- **`capsule/invoke` は使わない** — 既に 3 つの異なる shape で衝突中:
  - `crates/ato-cli/src/adapters/ipc/jsonrpc.rs:294` `InvokeParams { service, method, token, args }` (Service IPC)
  - `crates/ato-desktop/src/bridge.rs:467` `{ command, payload }` (Host → Guest backend HTTP)
  - 予定されていた guest stdio: `{ context, function_name?, input? }`
- **WASM/OCI 関連は defer** (v2.1 新規方針) — `capsule/wasm.execute` は本 PR では導入しない。
  ExecuteWasm は guest.v1 経路で引き続き利用可。WASM/OCI executor の新 env 注入も本 PR スコープ外。
- 本 PR で導入する method は **5 つの file IO のみ** (`capsule/payload.{read,write,update}`,
  `capsule/context.{read,write}`)
- **result/params は object wrapper で統一**: payload は `{ payload_b64: "..." }` 形
- **env 名は新命名規則 `CAPSULE_IPC_*` に統一** (v2.1 新規方針) — 旧名 (`CAPSULE_GUEST_PROTOCOL`,
  `GUEST_MODE`, `GUEST_ROLE`, `GUEST_WIDGET_BOUNDS`) は **破棄、fallback なし**。
  workspace 内に旧名を書く源コードは存在しないため安全に rename 可 (grep で確認済み)。
- **stdin は `read_to_string()` 維持** (single-shot model)
- **dispatch は `Result<Option<Value>, GuestError>` で型安全化**
- **エラーマップは jsonrpc.rs の既存定数のみ使用** (`-32000` は無いので `-32603` に倒す)

実装規模は **1.5 日** (v2 より縮小、wasm.execute と OCI/WASM executor 変更を defer したため)。

## 1. 設計決定 (3 軸、v2 確定値)

### 軸 1: 後方互換 — Envelope auto-detect

`stdin` の JSON 全体を 1 メッセージとして読み (`read_to_string()` 既存パターン維持)、
envelope shape で 2 経路に分岐:

```rust
let mut buf = String::new();
std::io::stdin().read_to_string(&mut buf)?;
let raw: Value = serde_json::from_str(buf.trim())
    .map_err(|e| emit_jsonrpc_parse_error(e))?;     // -32700, id: null

if raw.get("jsonrpc").and_then(|v| v.as_str()) == Some("2.0") {
    handle_jsonrpc_request(raw, sync_path)
} else if raw.get("version").and_then(|v| v.as_str()) == Some("guest.v1") {
    handle_legacy_request(raw, sync_path)           // 既存 handle_request
} else {
    // 不明 envelope → JSON-RPC 形で -32600 を返す
    write_jsonrpc_error_response(Value::Null, error_codes::INVALID_REQUEST,
        "Unknown envelope: expected jsonrpc=\"2.0\" or version=\"guest.v1\"")
}
```

両 field がある場合は `jsonrpc` 優先で扱う (CAPSULE_IPC_SPEC に明記する)。

### 軸 2: メソッド命名 (v2.1 確定 — 5 メソッドのみ、ExecuteWasm は defer)

| `GuestAction` (legacy) | JSON-RPC method (v2.1) | params 形 | result 形 |
|---|---|---|---|
| `ReadPayload` | `capsule/payload.read` | `{ context }` | `{ payload_b64: string }` |
| `WritePayload` | `capsule/payload.write` | `{ context, payload_b64: string }` | `null` |
| `UpdatePayload` | `capsule/payload.update` | `{ context, payload_b64: string }` | `null` |
| `ReadContext` | `capsule/context.read` | `{ context }` | `{ value: Value }` |
| `WriteContext` | `capsule/context.write` | `{ context, value: Value }` | `null` |
| `ExecuteWasm` | **(本 PR では未対応)** | — guest.v1 経路でのみ利用可 — | — |

#### ExecuteWasm を defer する根拠 (v2.1 新規方針)
- WASM/OCI ランタイム統合がまだ整っていない (ユーザ指示)
- `capsule/wasm.execute` という新 method を導入してすぐ使われない状態にするより、
  WASM ランタイム本体の整備を待ってから一括で公開するほうが衝突リスクが低い
- 既存の guest.v1 `ExecuteWasm` 経路は **無変更で残す** ため、現行ユーザ (テスト含む) は影響なし
- 将来の Phase でメソッド名を確定する余地を残せる (`capsule/wasm.execute` 以外も検討可能)

#### `capsule/invoke` 衝突回避の根拠 (参考)
本 PR で `capsule/invoke` は使わない。理由:
- `crates/ato-cli/src/adapters/ipc/jsonrpc.rs:294-303` で `InvokeParams { service, method, token, args }` を Service-to-Service 用に予約
- `crates/ato-desktop/src/bridge.rs:467` で `{ command, payload }` 形を Host-to-Guest-backend HTTP 用に使用中
- guest stdio に同名で別 shape を被せると将来の statically-typed dispatcher が破綻する

CAPSULE_IPC_SPEC の更新に「`capsule/invoke` は Service-to-Service の予約名」と明記する。

#### result の object wrapper 採用理由
- 旧 `ReadPayload` は base64 string をそのまま result とする (jsonrpc 化しても許容可だが)
- spec 互換性とフィールド追加の余地を考慮し、`{ payload_b64 }` / `{ value }` / `{ output }` の object 形に揃える
- write 系は `null` を `JsonRpcResponse::success(id, Value::Null)` で返す

### 軸 3: エラーコード対応 (v2 確定)

実 `jsonrpc.rs::error_codes` の既存定数のみを使う (新規定数は追加しない):

| `GuestErrorCode` | JSON-RPC code | 由来 |
|---|---|---|
| `PermissionDenied` | `-32001` | `PERMISSION_DENIED` |
| `InvalidRequest` | `-32602` | `INVALID_PARAMS` |
| `ExecutionFailed` | `-32603` | `INTERNAL_ERROR` (※ v1 案の `-32000` は定数なし) |
| `HostUnavailable` | `-32002` | `SERVICE_UNAVAILABLE` |
| `ProtocolError` | `-32600` | `INVALID_REQUEST` |
| `IoError` | `-32603` | `INTERNAL_ERROR` |

`GuestErrorCode` に `to_jsonrpc_code(&self) -> i64` impl を追加。
`data.hint` には旧来の `GuestError::message` を埋める。

## 2. 環境変数の統一 (v2.1 — 新命名で完全統一、fallback なし)

ユーザ指示に基づき、**fallback を捨てて旧名を破棄**。`CAPSULE_IPC_*` の新命名規則で統一する。

| 新 env (本 PR で唯一サポート) | 旧 env (削除) | 用途 |
|---|---|---|
| `CAPSULE_IPC_PROTOCOL` | ~~`CAPSULE_GUEST_PROTOCOL`~~ | プロトコル識別 (期待値 `"guest.v1"` または `"jsonrpc-2.0"`) |
| `CAPSULE_IPC_TRANSPORT` | (新規) | "stdio" 固定 |
| `CAPSULE_IPC_ROLE` | ~~`GUEST_ROLE`~~ | `"consumer"` / `"owner"` |
| `CAPSULE_IPC_MODE` | ~~`GUEST_MODE`~~ | `"widget"` / `"headless"` |
| `CAPSULE_IPC_SYNC_PATH` | ~~`SYNC_PATH`~~ (※注) | host 側の sync directory パス |
| `CAPSULE_IPC_WIDGET_BOUNDS` | ~~`GUEST_WIDGET_BOUNDS`~~ | widget mode の `x,y,width,height` |

### ※ `SYNC_PATH` のリネームについて

`SYNC_PATH` は guest.rs:267 の reader (host が設定する env) と、
guest.rs:521 / nacelle/src/sync.rs:249 の writer (WASI sandbox の virtual mount path `/sync`) で
同じ名前が **異なる文脈** で使われている:

- **host 側 env (reader)**: ato-desktop など caller が `ato guest` を起動する際に渡す
  → **`CAPSULE_IPC_SYNC_PATH` にリネーム**
- **WASI mount path (writer for wasm modules)**: wasmtime が wasm モジュールに見せる虚パス
  → **`SYNC_PATH=/sync` のまま維持** (WASI 規約として温存)

両者は別レイヤーのため、片方だけリネームしても不整合は起きない。

### リネームのリスク評価

`CAPSULE_GUEST_PROTOCOL`, `GUEST_MODE`, `GUEST_ROLE`, `GUEST_WIDGET_BOUNDS` を **書く側のソース**は
workspace 内に存在しない (grep で確認済み)。reader 側のみ修正で完結する。

ato-desktop の `dist/darwin-arm64/Ato Desktop.app/Contents/Helpers/ato` バイナリは旧名を含むが、
これは過去ビルドのアーティファクトであり、次回ビルドで自動更新される。

外部統合 (将来の third-party 統合) に対しては CHANGELOG / CAPSULE_IPC_SPEC で破壊変更を周知する。

### 読み取り規則 (v2.1)

```rust
// guest.rs の env 検証は新 env のみを参照
if let Ok(protocol) = std::env::var("CAPSULE_IPC_PROTOCOL") {
    // 期待値: "guest.v1" または "jsonrpc-2.0"
}
if let Ok(role) = std::env::var("CAPSULE_IPC_ROLE") { /* ... */ }
if let Ok(mode) = std::env::var("CAPSULE_IPC_MODE") { /* ... */ }
if let Ok(sync_path) = std::env::var("CAPSULE_IPC_SYNC_PATH") { /* ... */ }
let widget_bounds = std::env::var("CAPSULE_IPC_WIDGET_BOUNDS").ok();
```

env が unset の場合の挙動は現行通り (validation を skip)。

## 3. 実装プラン (3 ステップ、v2 改訂)

### ステップ A: 純粋 dispatch の抽出

```rust
// 新規: src/cli/commands/guest_dispatch.rs

pub(crate) struct GuestDispatchInput {
    pub action: GuestAction,
    pub context: GuestContext,
    pub input: Value,
}

pub(crate) type GuestDispatchResult = Result<Option<Value>, GuestError>;

pub(crate) fn dispatch_guest_action(
    sync_path: &Path,
    input: GuestDispatchInput,
) -> GuestDispatchResult {
    // 1. validate sync_path matches input.context.sync_path (既存ロジック)
    // 2. effective_permissions(env, manifest, input.context.permissions)
    // 3. ensure_permissions(input.action, input.context.role, &perms)
    //    ← signature を action+role+&perms に縮める (interface refactor)
    // 4. action dispatch
    //    ReadPayload     → Ok(Some(json!({ "payload_b64": base64(read) })))
    //    WritePayload    → write(); Ok(None)
    //    UpdatePayload   → update(); Ok(None)
    //    ReadContext     → Ok(Some(json!({ "value": read_context() })))
    //    WriteContext    → write_context(); Ok(None)
    //    ExecuteWasm     → guest.v1 経路でのみ呼ばれる (JSON-RPC 経路は受け付けない)
}
```

`Result<Option<Value>, GuestError>` 型で「両方 Some / 両方 None」状態を構造的に排除。
write 系は `Ok(None)`、read 系は `Ok(Some(...))`、失敗は `Err`。

### ステップ B: JSON-RPC 経路の追加

```rust
// 新規: src/cli/commands/guest_jsonrpc.rs

pub(crate) fn handle_jsonrpc_request(raw: Value, sync_path: &Path) -> Result<()> {
    let req: JsonRpcRequest = match serde_json::from_value(raw) {
        Ok(r) => r,
        Err(e) => return write_jsonrpc_error(Value::Null,
            error_codes::INVALID_REQUEST, e.to_string()),
    };
    if let Err(err) = req.validate() {                  // jsonrpc field check
        return write_jsonrpc_response(JsonRpcResponse::error(req.id.clone(), err));
    }

    let action = match method_to_action(&req.method) {
        Some(a) => a,
        None => return write_jsonrpc_response(JsonRpcResponse::error(
            req.id, JsonRpcError::method_not_found(&req.method))),
    };

    let dispatch_input = match parse_method_params(&req.method, &req.params) {
        Ok(input) => input,
        Err(jrpc_err) => return write_jsonrpc_response(
            JsonRpcResponse::error(req.id, jrpc_err)),  // -32602
    };

    let response = match dispatch_guest_action(sync_path, dispatch_input) {
        Ok(Some(value)) => JsonRpcResponse::success(req.id, value),
        Ok(None) => JsonRpcResponse::success(req.id, Value::Null),
        Err(guest_err) => JsonRpcResponse::error(req.id, jsonrpc_error_from_guest(guest_err)),
    };
    write_jsonrpc_response(response)
}

fn method_to_action(method: &str) -> Option<GuestAction> {
    match method {
        "capsule/payload.read"   => Some(GuestAction::ReadPayload),
        "capsule/payload.write"  => Some(GuestAction::WritePayload),
        "capsule/payload.update" => Some(GuestAction::UpdatePayload),
        "capsule/context.read"   => Some(GuestAction::ReadContext),
        "capsule/context.write"  => Some(GuestAction::WriteContext),
        // ExecuteWasm は本 PR 対象外 (WASM ランタイム整備待ち、guest.v1 経由でのみ利用可)
        _ => None,
    }
}

fn parse_method_params(method: &str, params: &Option<Value>)
    -> Result<GuestDispatchInput, JsonRpcError>
{
    let p = params.as_ref().ok_or_else(|| JsonRpcError::invalid_params(
        "Missing params object", "Send params with at least { context }"))?;
    let context: GuestContext = serde_json::from_value(
        p.get("context").cloned().ok_or_else(||
            JsonRpcError::invalid_params("Missing context",
                "All guest stdio methods require params.context"))?)
        .map_err(|e| JsonRpcError::invalid_params(&e.to_string(),
            "context must match GuestContext schema"))?;

    let input = match method {
        "capsule/payload.read" | "capsule/context.read" => Value::Null,
        "capsule/payload.write" | "capsule/payload.update" => {
            let b64 = p.get("payload_b64").and_then(|v| v.as_str()).ok_or_else(||
                JsonRpcError::invalid_params("Missing payload_b64",
                    "Provide payload as base64 string in payload_b64 field"))?;
            Value::String(b64.to_string())   // 既存 dispatcher が base64 string を期待
        }
        "capsule/context.write" => p.get("value").cloned().unwrap_or(Value::Null),
        // capsule/wasm.execute は本 PR 対象外 (method_to_action で None を返すため到達しない)
        _ => Value::Null,
    };

    let action = method_to_action(method).expect("method already validated");
    Ok(GuestDispatchInput { action, context, input })
}

fn jsonrpc_error_from_guest(e: GuestError) -> JsonRpcError {
    JsonRpcError::new(e.code.to_jsonrpc_code(), e.message.clone(), Some(e.message))
}

fn write_jsonrpc_response(resp: JsonRpcResponse) -> Result<()> {
    let json = serde_json::to_string(&resp)?;
    println!("{json}");
    Ok(())
}

fn write_jsonrpc_error(id: Value, code: i64, message: impl Into<String>) -> Result<()> {
    let err = JsonRpcError::new(code, message.into(), None);
    write_jsonrpc_response(JsonRpcResponse::error(id, err))
}
```

### ステップ C: env 名のクリーンリネーム + ensure_permissions interface 縮退

```rust
// guest.rs:232-325 の env reader を新名のみに書き換え (fallback 不要)
if let Ok(protocol) = std::env::var("CAPSULE_IPC_PROTOCOL") { /* ... */ }
if let Ok(role)     = std::env::var("CAPSULE_IPC_ROLE")     { /* ... */ }
if let Ok(mode)     = std::env::var("CAPSULE_IPC_MODE")     { /* ... */ }
if let Ok(sync)     = std::env::var("CAPSULE_IPC_SYNC_PATH"){ /* ... */ }
let widget = std::env::var("CAPSULE_IPC_WIDGET_BOUNDS").ok();

// ensure_permissions の signature 縮退 (interface refactor)
fn ensure_permissions(
    action: &GuestAction,
    role: &GuestContextRole,
    permissions: &GuestPermission,
) -> Result<(), GuestError> { /* ... */ }
```

**OCI/WASM executor の env 注入は本 PR 対象外** (ユーザ指示によりランタイム整備待ち)。
本 PR では Source 経路の env 名のみ対応。OCI/WASM 側は次 PR で同様に新名へ揃える。

## 4. テスト戦略 (v2 — レビュー指摘を反映)

### 既存温存
- `tests/guest_e2e.rs` (418 行) は `version: "guest.v1"` のまま温存、全通過

### 新規追加 (`tests/guest_jsonrpc_e2e.rs`)
1. `guest_jsonrpc_payload_read_returns_object_wrapper`
   — `result == { "payload_b64": "..." }`
2. `guest_jsonrpc_payload_write_returns_null_result`
3. `guest_jsonrpc_payload_update_returns_null_result`
4. `guest_jsonrpc_context_read_returns_value_wrapper`
5. `guest_jsonrpc_context_write_returns_null_result`
6. `guest_jsonrpc_wasm_execute_returns_method_not_found`
   — `capsule/wasm.execute` は本 PR 対象外、`-32601` を返すことを固定
7. `guest_jsonrpc_unknown_method_returns_minus_32601_with_id_echoed`
8. `guest_jsonrpc_invalid_params_returns_minus_32602`
   - `payload.write` の payload_b64 欠落
   - `payload.read` の context 欠落
9. `guest_jsonrpc_parse_error_returns_minus_32700_with_id_null`
10. `guest_jsonrpc_invalid_jsonrpc_version_returns_minus_32600`
11. `guest_jsonrpc_permission_denied_returns_minus_32001`
12. `guest_envelope_priority_jsonrpc_wins_when_both_fields_present`
13. `guest_unknown_envelope_returns_jsonrpc_minus_32600`

### env リネーム検証テスト
14. `guest_new_env_capsule_ipc_role_validated`
    — `CAPSULE_IPC_ROLE=owner` を request context.role と照合
15. `guest_old_env_guest_role_is_ignored`
    — `GUEST_ROLE=owner` を設定しても **無視** され、validation を skip する
    (旧名サポート無し方針の固定)

### ユニット
16. `method_to_action` の 6 ケース (5 method + unknown — `capsule/wasm.execute` も unknown)
17. `to_jsonrpc_code` の 6 ケース (各 GuestErrorCode)
18. `parse_method_params` の正常 + 各 invalid params パターン

## 5. ファイル変更見込み (v2.1)

| ファイル | 種類 | 変更内容 |
|---|---|---|
| `src/cli/commands/guest.rs` | 修正 | execute() で envelope 分岐、handle_legacy_request にリネーム、env reader を新名のみに変更 |
| `src/cli/commands/guest_dispatch.rs` | **新規** | `dispatch_guest_action` (純粋ロジック) + `ensure_permissions` 縮退 |
| `src/cli/commands/guest_jsonrpc.rs` | **新規** | `handle_jsonrpc_request`, `method_to_action`, `parse_method_params`, `jsonrpc_error_from_guest` |
| `src/adapters/ipc/guest_protocol.rs` | 修正 | `GuestErrorCode::to_jsonrpc_code()` impl 追加 |
| `tests/guest_jsonrpc_e2e.rs` | **新規** | 13 件の E2E + env リネーム 2 件 |
| `docs/specs/CAPSULE_IPC_SPEC.md` | 修正 | guest method 一覧 (5 件)、`capsule/invoke` 予約、`capsule/wasm.execute` は将来予約と明記、env 命名規則 |
| `docs/TODO.md` | 修正 | §13b.9 を `[x]` (`capsule/wasm.execute` defer の補足あり) |
| `crates/ato-cli/CHANGELOG.md` | 修正 | env リネームを破壊変更として明記 |

**本 PR では触らない (defer)**:
- `src/adapters/runtime/executors/wasm.rs` — WASM ランタイム整備待ち
- `src/adapters/runtime/executors/oci.rs` — OCI ランタイム整備待ち
- `src/cli/commands/open.rs` の executor 起動側 env 注入 — Source 以外は触らない

## 6. リスクと緩和 (v2.1)

| リスク | 重大度 | 緩和 |
|---|---|---|
| `capsule/wasm.execute` を本 PR で導入しないため TODO 表現と不一致 | L | TODO.md / CAPSULE_IPC_SPEC で「`capsule/wasm.execute` は WASM ランタイム整備後に確定、当面は guest.v1 のみ」と明記 |
| env リネームで外部統合 (古いビルドの ato-desktop binary 等) が動かなくなる | M | CHANGELOG / CAPSULE_IPC_SPEC で破壊変更を周知。`dist/` バイナリは次回ビルドで自動更新。grep で workspace 内に旧名 writer が無いことを確認済み |
| object wrapper 化で外部統合が壊れる | L | guest.v1 経路は無変更。新経路のみ wrapper 採用 |
| context.read result の `{ value: ... }` が legacy の生 JSON 値返却と非互換 | L | guest.v1 は object wrapper にしない (legacy のまま)。新経路のみ wrapper |
| `ExecutionFailed` を `-32603` にすることで Service IPC 系のエラーと混同 | L | data.hint で具体メッセージを返すため運用上区別可能 |
| OCI/WASM defer 中に新しい IPC 機能が必要になる | L | 対象を明示的に Source 経路に限定し、CAPSULE_IPC_SPEC で defer 範囲を明記 |

## 7. レビュー指摘 → v2 での対応

| # | 指摘 | v2 対応 |
|---|------|---------|
| 1 | `capsule/invoke` shape 衝突 | `capsule/wasm.execute` に逃がす + spec 明記 |
| 2 | result shape 未確定 | object wrapper に統一 (`{ payload_b64 }` 等) |
| 3 | parse error の `id: null` | `Value::Null` を明示的に渡す |
| 4 | `JsonRpcResponse::result` 不存在 | `success(id, value)` を使う、`Ok(None)` は `Value::Null` |
| 5 | env 互換表が現行コードと違う | 実コード基準 (`GUEST_MODE`, `GUEST_ROLE`, ...) に修正 |
| 6 | stdin 読み取りが現行と違う | `read_to_string()` 維持、line 単位は採らない |
| 7 | dispatch 型 | `Result<Option<Value>, GuestError>` に変更 |
| 8 | `ensure_permissions` interface | `(action, role, &permissions)` に縮退 |
| 9 | sync_path / context 必須を spec 化 | CAPSULE_IPC_SPEC に「stdio guest host-call では params.context 必須」と明記 |
| 10 | テストケース不足 | 9-15 の追加で 13 件 E2E + ユニット 3 種 |

## 8. 合意点 (v2.1 — ユーザ指示反映後)

1. **envelope auto-detect**: ✅ 採用
2. **method 命名**: ✅ 5 メソッドのみ採用 (`capsule/payload.{read,write,update}`, `capsule/context.{read,write}`)
3. **`capsule/invoke` ↔ ExecuteWasm**: ❌ 不採用 + ✅ **`capsule/wasm.execute` も本 PR では defer** (v2.1 変更)
4. **旧 env 維持**: ❌ **不採用に変更** (v2.1) — 新名 `CAPSULE_IPC_*` のみサポート、fallback なし
5. **deprecation 期限**: 🔄 「即削除」(v2.1 — workspace 内 writer 不在のため fallback 不要)
6. **ファイル分割**: ✅ `guest_dispatch.rs` / `guest_jsonrpc.rs` 分離

## 9. 実装フェーズ (v2.1 見積 1.5 日、scope 縮小)

```
Day 1 AM  ステップ A: dispatch 抽出 + ensure_permissions 縮退 + 既存テスト通過確認
Day 1 PM  ステップ B: handle_jsonrpc_request + parse_method_params + method_to_action (5 メソッド)
Day 2 AM  ステップ C: env reader を新名に rename + E2E 13 件 + env リネーム検証 2 件 + ユニット 3 種
Day 2 AM  仕上げ: CAPSULE_IPC_SPEC / TODO / CHANGELOG 更新 + commit
```

OCI/WASM executor の env 注入は別 PR (WASM/OCI ランタイム整備が完了次第) で実施。

## 10. 参考

### S-tier
- [JSON-RPC 2.0 Specification](https://www.jsonrpc.org/specification)
- [LSP 3.18 Specification](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.18/specification/)

### A-tier
- [Model Context Protocol Lifecycle](https://modelcontextprotocol.io/specification/2025-03-26/basic/lifecycle)

### ローカル参照 (v2 で確定)
- `crates/ato-cli/src/adapters/ipc/jsonrpc.rs:128-148` — `JsonRpcResponse::success/error` API
- `crates/ato-cli/src/adapters/ipc/jsonrpc.rs:294-303` — `InvokeParams { service, method, token, args }` (Service IPC 予約)
- `crates/ato-cli/src/adapters/ipc/jsonrpc.rs:37-58` — `error_codes` module (利用可能定数のみ)
- `crates/ato-cli/src/cli/commands/guest.rs:62-64` — `read_to_string()` パターン (現行維持)
- `crates/ato-cli/src/cli/commands/guest.rs:232-276` — env 名 (`CAPSULE_GUEST_PROTOCOL`, `GUEST_MODE`, `GUEST_ROLE`, `SYNC_PATH`, `GUEST_WIDGET_BOUNDS`)
- `crates/ato-desktop/src/bridge.rs:464-472` — `capsule/invoke` を `{ command, payload }` shape で送信中の実例
