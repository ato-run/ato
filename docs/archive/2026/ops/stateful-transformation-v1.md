# Stateful transformation v1 — E2E evidence

- 状態: **proof / E2E**（Linux Runtime 上で実行する証拠。production の提供機能を主張する文書ではない）。
- 対象: `ato pack` / `ato run` / `ato reform`（PR #1557, #1559）、Scenario Verifier v1（#1560, #1561）、および既存の `run_attempt → verify_observed_scenario`。
- 追加した semantic concept は無い。K は既存の `BoundContract`、scenario は既存の `ato.scenario/1` requirement のまま。

## 1. 何を証明するか

**同じ frozen Contract K（HTTP root + `ato.scenario/1`）** の下で、**別の source code** かつ **別の内部 DB schema** を持つ Derivation が、同じ scenario を満たすこと。

| 不変条件 | 期待 | 証拠 |
|---|---|---|
| K 不変 | candidate bundle の root Contract bytes が baseline と byte-identical、`ContractRef` 同一 | `report.json`、`candidate.capsule` |
| D が別 | `DerivationRef` が A と B で異なる | baseline receipt と reform receipt |
| source が別 | workspace source closure が異なる | `report.old_source_closure_refs` / `new_source_closure_ref` |
| scenario が同一 object | candidate の scenario ref が baseline の `ato.scenario/1` と一致（frozen bundle から carry） | candidate receipt の scenario observation、側car |
| 振る舞いが同一 | scenario が `satisfied`、canonical transcript digest が A と B で一致 | `receipt.observations[]`（`path = "scenario:crud"`、`digest`、`body_sha256`） |
| 観測実行 | 実 HTTP に対して login → invalid → create → read → update → unauthorized → delete → read-after-delete を実行 | scenario transcript sidecar |
| 既存証跡を壊さない | baseline bundle / receipt が byte-identical | SHA-256 比較 |

## 2. fixture 設計（`apps/cli/tests/fixtures/stateful-transformation/`）

| fixture | 内容 | 内部 storage |
|---|---|---|
| `source-a/` | baseline。v2 manifest が **full Contract**（HTTP root + scenario `crud`）を宣言 | in-memory JSON object（項目 `id/name/quantity`、`next_id`） |
| `source-b/` | 変換候補。manifest は **`[contract]` を宣言しない**（derivation-candidate）。K は frozen bundle から | in-memory SQLite（`users(id, username, created_at)` / `items(sku, name, qty, created_at)`） |
| `source-b-broken/` | source-b と同じだが authorization を無視する（cleared-cookie read が 200） | SQLite（同上） |
| `source-b-redefines-k/` | candidate manifest が **別の Contract**（root status 201）を宣言 | 実行されない |
| `scenarios/main.json` | 両 source で byte-identical な scenario object。baseline では K に content-addressed される | — |

- workspace は Run の sandbox で **read-only**（`/app`）なので、両実装とも storage を **process memory** に置く（A: JSON object、B: in-memory SQLite）。Run ごとに新 process なので scenario v1 の `fresh_instance` 隔離条件を満たし、state slot / instance snapshot も使わない（`validated.derivation.state` 空、`index.instance_snapshot_ref` なし）。
- DB schema の差は **HTTP から観測できない**。証拠側で機械可読に記録する（後述 `implementation_difference`）。

### scenario steps（`scenarios/main.json`）

1. `login` POST /login → 200（cookie）
2. `invalid-create` POST /items `{"name":"","quantity":-1}` → **400**
3. `create` POST /items `{"name":"first","quantity":2}` → 201、`/id` を capture
4. `read-created` GET /items/${item} → 200（name=first, quantity=2）
5. `update` PUT /items/${item} `{"name":"updated","quantity":3}` → 200 + body digest
6. `unauthorized-read` GET /items/${item}（cookie cleared）→ **401**
7. `delete` DELETE /items/${item} → 204
8. `read-after-delete` GET /items/${item} → **404**

## 3. Negative matrix

| ケース | コマンド | 期待 |
|---|---|---|
| behavior break | `ato reform bundle-a source-b-broken --out out-broken` | 非 0。`report.status = "rejected"`、`k_preserved = true`、`fully_satisfied = false`。candidate は書かれたが receipt は failed、scenario transcript は `out-broken/scenario-transcripts/crud.json` に残る |
| K redefinition | `ato reform bundle-a source-b-redefines-k --out out-redefines` | 非 0。`candidate_contract_mismatch`。`k_preserved = false`。`candidate.capsule` も `receipt.json` も書かれない |
| baseline 改変なし | すべてのケース後 | baseline bundle / receipt の SHA-256 が不変 |

## 4. evidence schema

`target/stateful-transformation-evidence/evidence.json`（`CARGO_TARGET_DIR` を尊重。CI は同 directory を artifact upload）:

```text
schema: "ato.stateful-transformation-evidence/1"
status: "running" | "complete"
fixtures: { source_a, source_b, source_b_broken, source_b_redefines_k, scenario }
implementation_difference: {
  source_a: { kind: "json_file", path, items.fields, users.fields },
  source_b: { kind: "sqlite", path, tables: { users: [...], items: [...] } },
  source_a_sha256, source_b_sha256, scenario_json_sha256
}
baseline: { bundle_sha256, contract_ref, derivation_ref, source_closure_ref,
            receipt_sha256, receipt_path, transcript_path,
            scenario: { id, ref, verifier_version, transcript_digest }, fully_satisfied }
reform:   { accepted, k_preserved, frozen_contract_ref, baseline_derivation_ref,
            new_derivation_ref, baseline_source_closure_ref, new_source_closure_ref,
            candidate_bundle_sha256, candidate_root_contract_ref,
            receipt_path, transcript_path, report_path,
            scenario_transcript_digest, fully_satisfied }
negative_broken_behavior: { fully_satisfied: false, exit_ok: false, error, candidate_written: true,
                             receipt_path, transcript_path, report_path }
negative_redefines_k: { exit_ok: false, error, k_preserved: false, candidate_written: false, report_path }
non_claims: [...]
```

`base target` の都合で証拠は build target 配下に置く。`--nocapture` で `stateful transformation evidence: <path>` を出力する。

## 5. 再現手順（Linux）

前提: bubblewrap、unprivileged userns 許可、`/opt/ato/toolchains/python/3.12.7`（CI と同じ）。

```console
$ cargo test -p ato-cli --test stateful_transformation -- --nocapture
$ cat "$CARGO_TARGET_DIR/stateful-transformation-evidence/evidence.json"
```

macOS では `#![cfg(target_os = "linux")]` により 0 tests（compile のみ）。**macOS での実行成功を主張しない。**

## 6. 主張しないこと

- **process restart を跨ぐ永続化は証明しない**（scenario v1 は fresh instance のみ。再起動後の state 継続は別機能）。
- scenario の網羅性（全 API / 全エラー / 全 schema 差分）は証明しない。この fixture は CRUD + 認可 + 入力エラーの代表経路のみ。
- 「同じ K」は「同じ機能」「セキュリティが向上した」を意味しない。K は観測の集合であり、DB schema は K の外にある。
- hosted / ato-api / staging の実行は対象外。Linux local Runtime の CLI E2E のみ。
- baseline fixture 同士の移植性・汎用性は主張しない（同一の観測仕様を満たす 2 実装の実測）。
