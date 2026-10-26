# Contract-preserving Derivation transformation v0 — E2E evidence

- 状態: **proof**（Linux CI で実測する E2E）。production の挙動・提供機能を主張する文書ではない。
- 対象: `ato` の `ato pack` / `ato run` / `ato reform`（PR2 #1557）と、既存の `run_attempt → verify_observed_candidate`。
- 追加した semantic concept は無い。Contract / Derivation / Receipt の既存の意味だけを使う。

## 1. 何を証明するか

同じ frozen Contract K の下で、**異なる source code から作った 2 つの Derivation** が同じ K を満たすこと。

| 不変条件 | 期待 | どこで示すか |
|---|---|---|
| `K_old == K_new` | 同一 `ContractRef`（canonical JCS bytes も同一） | `report.frozen_contract_ref`、`receipt_b.contract_ref` |
| K の出所 | **candidate manifest ではない**。bundle の root Contract object は frozen bundle の canonical Contract bytes そのもの | `build_transformation_candidate` / `build_derivation_candidate_bundle`、unit test |
| `D_old != D_new` | 異なる `DerivationRef` | `receipt_b.derivation_ref != receipt_a.derivation_ref` |
| `Source_old != Source_new` | 異なる workspace source closure | `report.old_source_closure_refs[0] != report.new_source_closure_ref` |
| 検証 | 新しい receipt が `fully_satisfied: true` | `out/receipt.json` |
| 既存の証跡を壊さない | 旧 bundle / 旧 receipt が byte-identical | 変換前後の SHA-256 比較 |
| 公開しない | `ato reform` は publish / activate / switch をしない | candidate はローカル directory のみ |

補足:

- K は **再生成も再 capture もされない**。候補 Derivation は frozen Contract body に直接 bind し、candidate manifest は K の出所にならない（`build_derivation_candidate_bundle`）。
- 候補 source の `capsule.toml` は `[contract]` を **省略できる**（derivation-candidate manifest）。宣言した場合だけ frozen K と canonical bytes / `contract_ref` を比較し、不一致はビルド・実行の前に `candidate_contract_mismatch` で拒否する。`ato pack` は従来どおり `[contract]` 必須。
- 判定者は既存の Rust verifier（`ato_formation::verify::verify_runtime` / `run_attempt`）だけで、v0 はそれに何も足していない。
- 同じ K を満たすことは「同じ機能」を意味しない。K は観測（HTTP status / body digest など）であり、この文書は K の外の性質を主張しない。

## 2. フィクスチャ

`apps/cli/tests/fixtures/transformation-v0/`

| fixture | 内容 |
|---|---|
| `source-a/` | Python stdlib `http.server.ThreadingHTTPServer` + `BaseHTTPRequestHandler`。`GET /` は `{"service":"transformation-v0","status":"ok"}\n` |
| `source-b/` | 独立実装 `socketserver.TCPServer` + raw HTTP request parsing。**同じ bytes** を返す |
| `source-b-broken/` | `source-b` と同じ manifest（同じ `body_digest`）だが、`{"service":"transformation-v0","status":"degraded"}\n` を返す |
| `source-c-contract-change/` | `source-a` 実装だが observation が `status = 201`（K が変わる） |

- すべて `schema = "ato.capsule/2"`、workspace input 1 つ、`[[contract.observation]]` 1 つ（`ato.contract.http@1`, `GET /`, status 200, `body_digest`）、`[[derivation]] use = "ato.process@1"`（`python3 app.py`, `guest_port = 8000`, `runtimes = { python = "3.12" }`）、`[effects] default = "pure"`。candidate source は `[contract]` を省略してもよい（省略時は frozen K に直接 bind する）。
- `body_digest` は `sha256:f345de2490fa7c64f7398b35c26b4591dfa662d3e7815c3a73e591f48559452d`（改行込みの完全一致）。
- K は **source identity を capture しない**（`ato.contract.workspace@1` を持たない）。これが「別 source を同じ K で受けられる」条件であり、source-bound K の拒否は §3 の negative 3 で示す。
- listen port は既存の process fixture と同じく `ATO_ENDPOINT_APP_HTTP_PORT` を使う。binding は `127.0.0.1`。

## 3. Negative matrix

| ケース | 入力 | 期待 | 記録 |
|---|---|---|---|
| behavior break | `ato reform bundle-a source-b-broken --out out-broken` | 非 0 終了。`report.status = "rejected"`、`k_preserved = true`、`fully_satisfied = false`。失敗した observation が `failed` で残る | `out-broken/report.json`, `out-broken/receipt.json`（`fully_satisfied:false`） |
| contract change | `ato reform bundle-a source-c-contract-change --out out-contract` | 非 0 終了。`candidate_contract_mismatch`。`k_preserved = false`。**`candidate.capsule` も `receipt.json` も書かれない**（ビルド前に拒否） | `out-contract/report.json` |
| source-bound K | `ato reform samples/interop-static.capsule source-b --out out-source-bound` | 非 0 終了。`frozen_contract_is_source_bound`。`k_preserved = false`。何もビルド・実行しない | `out-source-bound/report.json` |
| candidate manifest without `[contract]` | `ato reform bundle-a source-b-no-contract --out out-no-contract` | ビルドされ、bundle root の Contract bytes は frozen K と byte-identical。実行/検証は通常どおり | `out-no-contract/candidate.capsule`, `report.json` |

どのケースでも旧 bundle は byte-identical のまま。`ato reform` は publish / activate / switch を行わない。

## 4. 再現手順（Linux）

前提（CI の `rust-ci.yml` が用意するものと同じ）:

- Linux（bubblewrap と、unprivileged userns を許可した AppArmor 設定）
- Python **3.12.7** が `/opt/ato/toolchains/python/3.12.7` に配置済み（`runtimes = { python = "3.12" }` の解決先）
- リポジトリ root で作業

```console
# 1. テスト（= 証跡生成）
$ cargo test -p ato-cli --test derivation_transformation -- --nocapture

# 2. 証跡
$ cat target/transformation-v0-evidence/evidence.json
$ ls target/transformation-v0-evidence/

# 3. 手で同じ経路をたどる場合
$ cargo build -p ato-cli
$ target/debug/ato pack apps/cli/tests/fixtures/transformation-v0/source-a -o /tmp/bundle-a.capsule
$ target/debug/ato run /tmp/bundle-a.capsule --no-open --verification-receipt /tmp/receipt-a.json
$ target/debug/ato reform /tmp/bundle-a.capsule \
    apps/cli/tests/fixtures/transformation-v0/source-b --out /tmp/out
$ cat /tmp/out/report.json /tmp/out/receipt.json
```

- 証跡 directory は `CARGO_TARGET_DIR` が設定されていればその下、無ければ `<repo>/target/transformation-v0-evidence/`。
- `ato run` / `ato reform` はブラウザを開かない。`--no-open` は `ato run` のみ。

## 5. Evidence schema

`target/transformation-v0-evidence/evidence.json`（schema `ato.derivation-transformation-evidence/1`）:

| field | 内容 |
|---|---|
| `schema` | `ato.derivation-transformation-evidence/1` |
| `fixtures` | 4 fixture の path |
| `baseline.bundle_a_sha256` / `receipt_a_sha256` | 変換前の SHA-256 |
| `baseline.contract_ref` / `derivation_ref` / `source_closure_ref` | K_A / D_A / source closure A |
| `baseline.fully_satisfied` | `true` |
| `baseline.bundle_a_sha256_after_reform` / `receipt_a_sha256_after_reform` | 変換後（同一であることを比較） |
| `baseline.old_bundle_unchanged` / `old_receipt_unchanged` | `true` |
| `transformation.report_path` / `receipt_path` | 証跡 directory に copy した JSON の path |
| `transformation.candidate_bundle_sha256` / `candidate_root_contract_ref` | candidate transport と、その bundle の root（= K_A） |
| `transformation.new_derivation_ref` / `new_source_closure_ref` | D_B / source closure B |
| `transformation.k_equal` / `derivation_changed` / `source_changed` | `true` / `true` / `true` |
| `transformation.fully_satisfied` / `status` | `true` / `accepted` |
| `transformation.observations` | 新しい receipt の observation 一覧（既存 receipt schema のまま） |
| `negatives.behavior_break_rejected` | `true`（`behavior_break_report_path` / `behavior_break_receipt_path`） |
| `negatives.contract_change_refused` | `true`（`contract_change_report_path`, `contract_change_error`） |
| `negatives.source_bound_refused` | `true`（`source_bound_report_path`, `source_bound_error`） |
| `status` | `passed`（完走時） |

- `evidence.json` は phase ごとに上書き保存する。途中で assertion が落ちても、それまでの phase が CI artifact に残る。
- 同じ directory に `out--report.json` / `out--receipt.json` / `out-broken--*.json` / `out-contract--report.json` / `out-source-bound--report.json` を copy する。

## 6. CI

`.github/workflows/rust-ci.yml` の Linux job に追加（既存 step は変更しない）:

- `Derivation transformation v0 E2E (Linux)`: `cargo test -p ato-cli --test derivation_transformation --locked -- --nocapture`
- `Upload transformation v0 evidence (Linux)`: `actions/upload-artifact@v4` で `target/transformation-v0-evidence/` を常に upload（artifact 名 `transformation-v0-evidence`）

Linux 以外では統合テストは `#![cfg(target_os = "linux")]` により 0 件（コンパイルのみ）。pure な unit test（preset の source-bound K が closure ごとに異なること）は全 OS で実行される。

## 7. 主張しないこと

- **publish / activate / switch / auto-rollout は無い**。候補はローカル directory に書かれるだけ。
- **hosted / ato-api の E2E は無い**（ato-api への受理・D1 記録は別 PR）。
- **scenario verifier / functional acceptance は無い**（K の観測だけ）。
- 「同じ K」は「同じ機能」「脆弱性が直った」を意味しない。
- source-bound K（`ato.contract.workspace@1` を capture する K）は **別 source を受けない**。これは `source_changed_same_k` の別機能であり、v0 では意図的に拒否する（弱めない）。
- Instance snapshot を持つ frozen v4 bundle は v0 では **明示的に拒否**する（`frozen_instance_snapshot_unsupported`）。snapshot closure・portability manifest を候補へ再構成するのは Scenario PR の範囲。
- v0 の candidate transport は v3。v4 frozen bundle のうち snapshot を持たないものは受け付けるが、出力は v3 になる。
- 実行は Linux の local Runtime のみ。macOS / Windows の実行は未検証。
