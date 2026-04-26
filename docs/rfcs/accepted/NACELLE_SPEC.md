---
title: "Nacelle Engine Spec (v0.3)"
status: accepted
date: "2026-02-17"
author: "@egamikohsuke"
ssot:
  - "apps/nacelle/"
related:
  - "ATO_CLI_SPEC.md"
  - "CAPSULE_CORE.md"
---

# Nacelle Engine Spec (nacelle)

## 1. 概要

- Capsule を実行するためのエンジン（internal runtime）。Source ランタイムを担当。
- `ato-cli` から JSON over stdio で呼び出される。
- **Sandbox Enforcer** として、OS レベルの隔離ポリシーを適用する。IPC の _内容_（ルーティング・Token・Schema）には関与せず、_隔離_ のみ担当。

> **v0.3 Breaking Change:** IPC Broker 責務を ato-cli に移行。
> nacelle は Source ランタイムの Sandbox Enforcer に専念する（Smart Build, Dumb Runtime）。

## 2. 目的 / スコープ

- **目的:** 安全に Capsule を実行・監視する実行基盤を提供。
- **スコープ内:**
  - バンドル/アーティファクトの展開
  - OS ネイティブ隔離（Landlock / Seatbelt）
  - プロセス起動・監視（Supervisor Mode）
  - Socket Activation
  - **Sandbox Enforcer for IPC:** ato-cli が注入した IPC 用 Transport （Unix Socket 等）を Sandbox ポリシーで許可する
  - **Guest Manager:** Guest プロセスのモード管理・権限ゲート
- **非スコープ:**
  - 署名検証/UX/ポリシー意思決定（Smart Build, Dumb Runtime）
  - **IPC Broker** (Service 解決, RefCount, Token, Schema 検証) — **ato-cli の責務**
  - IPC の UI（ato-desktop の責務）

## 3. Engine Interface（CLI ↔ Engine）

- `nacelle internal features`
- `nacelle internal exec`
- `nacelle internal pack` (legacy placeholder; always returns `UNSUPPORTED`)

### 3.1 I/O

- stdin: JSON
- stdout: NDJSON
  - `internal features`: 単一 JSON response
  - `internal exec`: 1 行目に initial response、その後に event を 0 個以上流す
- stderr: human logs

`internal exec` の stdout contract:

1. initial response
   - `{"ok":true,"spec_version":"1.0","pid":12345,...}`
2. event stream（必要時）
   - `{"event":"ipc_ready","service":"main","endpoint":"tcp://127.0.0.1:43123"}`
   - `{"event":"service_exited","service":"main","exit_code":0}`

注記: 上記は nacelle の内部I/O契約（CLI ↔ engine）であり、利用者/エージェント向けの外部診断契約は
`ato-cli` 側で正規化したうえで `ATO_ERROR_CODES.md` に従う。

### 3.2 Exit Code

- `0`: success
- `1`: general failure
- `2`: invalid input
- `10`: policy violation

### 3.3 `spec_version`

- current: `1.0`
- next: `2.0`
- legacy compatibility: `0.1.0`
- それ以外は `ok=false` / `error.code="UNSUPPORTED"` で拒否する

> `internal_api.rs:3-5` — `CURRENT_SPEC_VERSION = "1.0"`, `NEXT_SPEC_VERSION = "2.0"`, `LEGACY_SPEC_VERSION = "0.1.0"`

## 4. 主要責務

- Workload 実行（source/bundle）
- Sandbox（Linux Landlock / macOS Seatbelt 等）
- Supervisor / signal forwarding
- JIT Provisioning

## 5. IPC における nacelle の役割 (Sandbox Enforcer)

IPC Broker は ato-cli が担う（CAPSULE_IPC_SPEC v1.1）。nacelle は **Sandbox Enforcer** として以下のみ担当する:

### 5.1 IPC Transport の Sandbox 許可

- ato-cli が生成した IPC 用の Unix Socket / Named Pipe パスを Sandbox ポリシーで許可する
- Seatbelt (macOS) / Landlock (Linux) プロファイルに IPC パスを動的に追加

### 5.2 環境変数の透過

- ato-cli から渡された `CAPSULE_IPC_*` 環境変数を子プロセスに透過させる
- nacelle 自身は環境変数の内容を解釈しない（Dumb Runtime）
- `user_secret` は env 透過対象外とし、FD 受け渡し前提で扱う（`session_token` は短命・allowlist 管理下で env 透過可）

### 5.3 Readiness Probe の報告

- `[services.*]` の `readiness_probe` 結果を ato-cli に報告する（JSON over stdout）
- ato-cli (IPC Broker) が readiness を待ってからClient への環境変数注入を行う

### 5.4 Process Isolation

- Sandbox ポリシーにより `/proc/*/environ` 等の他プロセス環境変数読み取りを防止
- IPC Token が他の Capsule プロセスから漏洩しないことを保証

## 6. Guest Manager

### 6.1 概要

- Guest プロセス（App-in-App）のライフサイクルとモードを管理する
- **現行実装:** `src/guest.rs` (291行) に `GuestManager` として実装済み

### 6.2 モード管理

- `widget` / `headless` / `consumer` / `owner` モードの切替
- `enable_widget_mode()`, `enable_headless_mode()` 等のメソッド

### 6.3 権限ゲート

- `allow_read()`, `allow_write()`, `allow_list()`, `allow_exec()` で個別権限を付与
- Guest のアクション実行は許可された権限の範囲内のみ

### 6.4 Orphan Protection

- Guest プロセスは `kill_on_drop(true)` で管理
- Host (ato-desktop) クラッシュ時に Guest も道連れ終了。orphan process は発生しない

### 6.5 IPC 統合 (v0.3+)

- Guest Protocol を JSON-RPC 2.0 (`capsule/invoke`, `capsule/ui.modeChange` 等) に移行
- `capsule/ui.modeChange` 要求を受け取った場合、ato-desktop にユーザー確認を委譲

## 7. セキュリティ

- 実行時の FS / Network policy enforce
- `ato-cli` から渡されたポリシーを適用
- **IPC Sandbox:** ato-cli が生成した IPC Transport パスを Sandbox で許可し、それ以外のネットワークアクセスを拒否
- **Process Isolation:** Sandbox ポリシーにより `/proc/*/environ` 等の他プロセス環境変数読み取りを防止

## 8. 実装状況

| 機能                        | 状態      | ファイル                                            |
| --------------------------- | --------- | --------------------------------------------------- |
| Supervisor Mode             | ✅ 実装済 | `src/manager/supervisor.rs` (761行)                 |
| SupervisorModePlan (DAG)    | ✅ 実装済 | `src/manager/supervisor_mode.rs` (862行)            |
| R3 Supervisor               | ✅ 実装済 | `src/manager/r3_supervisor.rs`                      |
| GuestManager                | ✅ 実装済 | `src/guest.rs` (291行)                              |
| IPC Sandbox 許可            | △ 部分実装 | `internal exec` / Source Runtime で IPC path allow-list を適用 |
| Readiness Probe 報告        | ✅ 実装済 | `internal exec` と supervisor が `ipc_ready` / `service_exited` を emit |
| JSON-RPC 2.0 Guest Protocol | ❌ 未着手 | 現行は独自プロトコル                                |

## 9. 関連ドキュメント

- Engine Contract: [nacelle/docs/ENGINE_INTERFACE_CONTRACT.md](../nacelle/docs/ENGINE_INTERFACE_CONTRACT.md)
- Security: [nacelle/SECURITY.md](../nacelle/SECURITY.md)
- **IPC Specification:** (DRAFT_CAPSULE_IPC.md は未確定、`ATO_CLI_SPEC.md` Section 2 を参照)
