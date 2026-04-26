# Engine Interface Contract

このドキュメントは、`ato-cli` が `nacelle` を engine process として呼び出すときの最小契約を定義する。

## 1. 基本原則

- 入力は JSON、出力は machine-readable な JSON / NDJSON
- 人間向けログは stderr に限定する
- 失敗時も stdout の先頭行は machine-readable な error response にする
- `spec_version` は request / response で必須

## 2. 対応コマンド

```bash
nacelle internal --input - features
nacelle internal --input - exec
nacelle internal --input - pack
```

`internal pack` は legacy compatibility 用の placeholder として受理するが、常に
`ok=false` / `error.code="UNSUPPORTED"` を返す。build / packaging の責務は `ato-cli` にある。

## 3. `spec_version`

`spec_version` は request schema version として厳格に扱う。未知 version は best-effort せず fail-closed にする。

現行実装が受け付ける version は次の 3 つ:

- `1.0` : current
- `2.0` : declarative environment contract
- `0.1.0` : legacy compatibility

それ以外は `ok=false` / `error.code="UNSUPPORTED"` で fail-closed にする。

## 4. `internal features`

### request

```json
{ "spec_version": "1.0" }
```

### response

```json
{
  "ok": true,
  "spec_version": "1.0",
  "engine": {
    "name": "nacelle",
    "engine_version": "0.2.8",
    "platform": "darwin-aarch64",
    "commit": null
  },
  "capabilities": {
    "workloads": ["source", "bundle"],
    "languages": ["python", "node", "deno", "bun"],
    "sandbox": ["macos-seatbelt"],
    "socket_activation": true,
    "jit_provisioning": true,
    "ipc_sandbox": true
  }
}
```

### contract notes

- `sandbox` は compile target ではなく runtime backend 可用性ベース
- backend が 1 つも無い場合、`sandbox=[]` かつ `ipc_sandbox=false`
- `languages` は `python` / `node` / `deno` / `bun` を返す
- macOS backend は `macos-seatbelt` のみを返す

## 5. `internal exec`

### request

```json
{
  "spec_version": "1.0",
  "workload": {
    "type": "source",
    "manifest": "/abs/path/to/capsule.toml"
  },
  "env": [["PORT", "43123"]],
  "ipc_env": [["CAPSULE_IPC_FOO_URL", "unix:///tmp/foo.sock"]],
  "ipc_socket_paths": ["/tmp/foo.sock"]
}
```

### request (`spec_version = "2.0"`)

```json
{
  "spec_version": "2.0",
  "workload": {
    "type": "source",
    "environment_spec": {
      "lower_source": {
        "manifest": "/abs/path/to/capsule.toml"
      },
      "upper_overlays": [
        {
          "source": "/abs/path/to/generated.env",
          "target": ".env",
          "readonly": true
        }
      ],
      "derived_outputs": [
        {
          "host_path": "/abs/path/to/derived-output",
          "target": ".derived",
          "kind": "artifact"
        }
      ],
      "runtime_artifacts": [
        {
          "name": "python",
          "path": "/abs/path/to/python3",
          "env_var": "NACELLE_RUNTIME_ARTIFACT_PYTHON",
          "add_to_path": true
        }
      ]
    }
  },
  "env": [["PORT", "43123"]],
  "ipc_env": [["CAPSULE_IPC_FOO_URL", "unix:///tmp/foo.sock"]],
  "ipc_socket_paths": ["/tmp/foo.sock"],
  "cwd": "."
}
```

### v2 environment semantics

- `lower_source` は実行時の基準ワークスペースで、元のホストパスは不変として扱う
- `upper_overlays` は workspace root からの相対 target に重ねる
- `derived_outputs` は workspace root からの相対 target に write target を注入し、`host_path` は lower_source 配下を指してはいけない
- `runtime_artifacts` は `ato-cli` が解決済みの参照のみを渡す。`nacelle` は解決せず、存在検証と env/PATH 注入だけを行う

### stdout contract

`internal exec` は stdout を NDJSON として使う。

1 行目は常に initial response:

```json
{
  "ok": true,
  "spec_version": "1.0",
  "pid": 12345,
  "log_path": null
}
```

2 行目以降は 0 個以上の event:

```json
{"event":"ipc_ready","service":"main","endpoint":"unix:///tmp/foo.sock"}
{"event":"service_exited","service":"main","exit_code":0}
{"event":"execution_completed","service":"main","run_id":"exec-12345","derived_output_path":"/abs/path/to/derived-output","exported_artifacts":[{"kind":"artifact","relative_path":"result.txt","size_bytes":42}],"cleanup_policy_applied":"delete_workspace_preserve_outputs","exit_code":0}
```

### event types

- `ipc_ready`
  - readiness probe 成功時に送る
  - `endpoint` は `unix://...` または `tcp://...`
  - `port` は TCP readiness のときのみ付与してよい
- `service_exited`
  - service が終了したときに送る
  - `exit_code` は取得できる場合のみ数値
- `execution_completed`
  - 実行の最終サマリ
  - `run_id` は engine 内の一意識別子
  - `derived_output_path` は primary output root がある場合のみ付与する
  - `exported_artifacts[]` は `kind` / `relative_path` / `size_bytes` を返す
  - `cleanup_policy_applied` は engine が適用した cleanup policy を返す

### ordering

- initial response の前に event を出してはいけない
- readiness 前に service が落ちた場合は `ipc_ready` を出さず、`service_exited` のみを出す
- `execution_completed` は `service_exited` の後に出す

## 6. `internal pack`

### request

```json
{ "spec_version": "1.0" }
```

### response

```json
{
  "ok": false,
  "spec_version": "1.0",
  "error": {
    "code": "UNSUPPORTED",
    "message": "internal pack is not supported by nacelle. Packaging/build is owned by ato-cli",
    "details": null
  }
}
```

## 7. 共通 response schema

成功:

```json
{
  "ok": true,
  "spec_version": "1.0"
}
```

失敗:

```json
{
  "ok": false,
  "spec_version": "1.0",
  "error": {
    "code": "INVALID_INPUT",
    "message": "manifest path is required",
    "details": null
  }
}
```

## 8. 推奨 error.code

- `INVALID_INPUT`
- `UNSUPPORTED`
- `POLICY_VIOLATION`
- `INTERNAL`

## 9. Exit Code

- `0`: success
- `1`: general failure
- `2`: invalid input
- `10`: policy violation

実装上まだ細かな分類は発展途上だが、stdout contract は上記 schema に固定する。

## 10. Discovery

`ato-cli` は次の順で engine を探してよい。

1. `NACELLE_PATH`
2. `$PATH` 上の `nacelle`
3. `~/.capsule/engines/nacelle/<version>/nacelle`
