# Engine Interface Contract (Draft)

このドキュメントは、**capsule（メタレイヤー/CLI）** が **nacelle（実行エンジン）** をプロセス境界で呼び出すための最小契約（案）です。

目的:
- 疎結合（Unix哲学）: engine は単機能・stdin/stdout で完結
- 多態性（Polymorphism）: 将来の `capsule-wasm` / `capsule-oci` 等へ同じ契約で差し替え可能
- 安定化: capsule は頻繁に更新、engine はLTS志向でも成立

非目的:
- engine の内部実装詳細（Supervisor/Sandbox/JIT等）を規定しない
- 高度なUX（watch、GUI等）をこの契約に詰め込まない

---

## 1. 基本原則

- **I/O**: JSON を stdin から受け取り、JSON を stdout に返す
- **ログ**: 人間向けログは stderr（capsule がそのまま表示/保存できる）
- **互換性**: `spec_version` で契約バージョンを明示し、後方互換を維持
- **エラー**: 失敗時は stdout に `ok=false` の JSON を出し、exit code も非0
- **シグナル**: `exec` は SIGINT/SIGTERM を受けて子へ転送し、終了コードで状態を返す

---

## 2. コマンド体系（最小セット）

capsule は engine バイナリ（例: `nacelle`）を次の形式で起動します。

```bash
nacelle internal --input - <command>
```

- `--input -` は stdin から JSON payload を読む（ファイルパスも許容してよい）

### 2.1 `internal features`
**目的**: engine の能力を列挙し、capsule がディスパッチ可否判断に使う。

```bash
nacelle internal --input - features
```

入力（空JSONで可）:
```json
{ "spec_version": "0.1.0" }
```

出力例:
```json
{
  "ok": true,
  "spec_version": "0.1.0",
  "engine": {
    "name": "nacelle",
    "engine_version": "0.1.0",
    "commit": "<optional>",
    "platform": "darwin-aarch64"
  },
  "capabilities": {
    "workloads": ["source", "bundle"],
    "languages": ["python"],
    "sandbox": ["macos-seatbelt", "linux-landlock"],
    "socket_activation": true,
    "jit_provisioning": true
  }
}
```

### 2.2 `internal pack`
**目的**: capsule が用意した workload を受け取り、配布可能な成果物を生成する。

```bash
nacelle internal --input - pack
```

入力（例）:
```json
{
  "spec_version": "0.1.0",
  "workload": {
    "type": "source",
    "path": "./app",
    "manifest": "./app/capsule.toml"
  },
  "output": {
    "format": "bundle",
    "path": "./nacelle-bundle"
  },
  "runtime_path": null,
  "options": {
    "sign": false
  }
}
```

出力（例）:
```json
{
  "ok": true,
  "spec_version": "0.1.0",
  "artifact": {
    "format": "bundle",
    "path": "./nacelle-bundle"
  }
}
```

### 2.3 `internal exec`
**目的（最重要）**: workload を実行する。Socket Activation の FD 継承、Supervisor、Sandbox、シグナル転送を engine 側で実施する。

```bash
nacelle internal --input - exec
```

入力（例）:
```json
{
  "spec_version": "0.1.0",
  "interactive": true,
  "workload": {
    "type": "source",
    "path": "./app",
    "manifest": "./app/capsule.toml",
    "entrypoint": "main.py"
  },
  "runtime": {
    "name": "python",
    "version_constraint": ">=3.11"
  },
  "policy": {
    "network": "allow-outbound",
    "fs_allow": ["./data"],
    "sandbox": "best-effort"
  },
  "resources": {
    "env": { "PORT": "8080" },
    "sockets": [3]
  }
}
```

`exec` は 2 モードを持ちます（A案の「土管」要件対応）:

- `interactive=true`（推奨: `capsule dev`）
  - **stdout/stderr をアプリのログとしてストリーミング**する
  - **stdout に JSON を出さない**（ログ汚染を避ける）
  - 終了状態は **engine プロセスの exit code** で返す

- `interactive=false`（RPC的）
  - stdout に JSON を返す（下の例）

出力例（`interactive=false`）:
```json
{
  "ok": true,
  "spec_version": "0.1.0",
  "result": {
    "status": "exited",
    "exit_code": 0,
    "pid": 12345
  }
}
```

`exec` は原則 **フォアグラウンド**（呼び出し元が待つ）とし、バックグラウンド化は将来拡張で扱う。

---

## 3. 共通レスポンス形式

成功/失敗を最低限これで統一します。

```json
{
  "ok": true,
  "spec_version": "0.1.0"
}
```

失敗時:
```json
{
  "ok": false,
  "spec_version": "0.1.0",
  "error": {
    "code": "INVALID_INPUT",
    "message": "...",
    "details": { }
  }
}
```

### 推奨 `error.code`
- `INVALID_INPUT`
- `UNSUPPORTED`
- `NOT_FOUND`
- `POLICY_VIOLATION`
- `RUNTIME_MISSING`
- `INTERNAL`

---

## 4. Exit Code（推奨）

- `0`: `ok=true` かつ workload が成功終了
- `1`: 一般的失敗（`ok=false` / `INTERNAL` など）
- `2`: 入力不正（`INVALID_INPUT`）
- `10`: ポリシー違反（`POLICY_VIOLATION`）
- `128+N`: シグナルによる終了（慣習）

※ `exec` の場合:
- `interactive=true` では **stdout JSON は出さず**、workload（または engine）が終了コードで返す。
- `interactive=false` では stdout に JSON を返す。

---

## 5. Engine Discovery（capsule 側の推奨）

capsule は engine 探索を次の優先順で行う（案）:
1. 環境変数: `NACELLE_PATH`（将来的に `CAPSULE_ENGINE_PATH` 等へ一般化可）
2. `$PATH` 上の `nacelle`
3. `~/.capsule/engines/nacelle/<version>/nacelle`（JIT install 先）

---

## 6. バージョニング方針

- `spec_version` は契約（このドキュメント）に対して SemVer
- engine 実装バージョンは `engine.engine_version` で別管理
- capsule は `spec_version` の互換範囲で engine を選ぶ

---

## 7. 今後の拡張候補（後回し）

- `internal validate`（pack/exec 前の静的検査のみ）
- `internal logs`（exec の構造化ログストリーム）
- `internal exec --mode=daemon`（常駐/監視/再起動ポリシー）
