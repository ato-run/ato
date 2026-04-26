---
title: "ExecutionPlan 隔離実行モデル（Architecture Overview）"
status: accepted
date: "2026-02-23"
author: "@egamikohsuke"
ssot: []
related:
  - "EXECUTIONPLAN_ISOLATION_SPEC.md"
  - "EXECUTIONPLAN_CANONICALIZATION_SPEC.md"
---

# ExecutionPlan 隔離実行モデル（Architecture Overview）

**Status:** Informative (Non-Normative)

## 1. 目的

本ドキュメントは、現在の議論で合意した以下を明文化する。

- Ato の隔離実行アーキテクチャ（Control Plane / Data Plane）
- `ExecutionPlan` による実行前判定と同意モデル
- Tier1 / Tier2 の実行ポリシー
- 思考実験ケースでの期待動作（正常系・異常系）

本書はアーキテクチャ解説であり、規範的契約（MUST/互換性ルール）の正本は
`EXECUTIONPLAN_ISOLATION_SPEC.md` とする。

---

## 2. アーキテクチャ概要

### 2.1 Control Plane（ato-cli）

`ato-cli` は以下を担当する。

- `capsule.toml` -> `ExecutionPlan` 正規化
- Provisioning / Runtime の2フェーズ分離
- 同意判定（`policy_segment_hash` ベース）
- Host pre-flight（OS/arch/libc/CPU/sandbox可用性）
- Driver への引数コンパイル
- 失敗時の診断コード化（OS生エラーの直接露出を避ける）

### 2.2 Data Plane（Driver / Enforcer）

実行エンジンは `ExecutionPlan` を消費する。

- Tier1: `deno`, `wasmtime`, `browser_static`
- Tier2: `nacelle`（native best-effort/strict capability-gated）

`nacelle` は「ポリシー意思決定」ではなく「実行・隔離適用」の責務に限定する。

---

## 3. Tier モデル

### 3.1 Tier1（Strict）

対象:

- `runtime=web, driver=browser_static`
- `runtime=source, driver=deno`
- `runtime=wasm, driver=wasmtime`

原則:

- 常時 `fail-closed`
- lock/キャッシュ整合性が満たせなければ起動拒否
- 未同意なら非対話環境で即失敗

### 3.2 Tier2（Native）

対象:

- `runtime=source, driver=native`

原則:

- 明示 opt-in（`--unsafe` もしくは事前同意済み設定）なしでは拒否
- sandbox backend 不可用時は降格実行せず停止
- 実行前 pre-flight 検証必須（互換性不足なら起動前に fail-closed）

---

## 4. ExecutionPlan（確定方針）

`ExecutionPlan` は単一ルートオブジェクトとし、`provisioning` と `runtime` を明示的に分離する。

```json
{
  "schema_version": "1",
  "capsule": { "scoped_id": "publisher/slug", "version": "1.2.3" },
  "target": {
    "label": "cli",
    "runtime": "source",
    "driver": "deno"
  },
  "provisioning": {
    "network": { "allow_registry_hosts": ["deno.land", "registry.npmjs.org"] },
    "lock_required": true,
    "integrity_required": true
  },
  "runtime": {
    "policy": {
      "network": { "allow_hosts": ["api.openai.com"] },
      "filesystem": { "read_only": ["./public"], "read_write": ["./output"] },
      "secrets": { "allow_secret_ids": ["OPENAI_API_KEY"], "delivery": "fd" }
    },
    "fail_closed": true,
    "non_interactive_behavior": "deny_if_unconsented"
  },
  "consent": {
    "key": {
      "scoped_id": "publisher/slug",
      "version": "1.2.3",
      "target_label": "cli"
    },
    "policy_segment_hash": "blake3:...",
    "provisioning_policy_hash": "blake3:..."
  },
  "reproducibility": {
    "platform": { "os": "linux", "arch": "x86_64", "libc": "glibc-2.39" }
  },
  "secrets": { "mode": "byok" },
  "extensions": {}
}
```

`tier` は保存フィールドではなく `target.runtime + target.driver` からの派生値とする。
派生結果が Tier ルールと矛盾する入力は `ATO_ERR_POLICY_VIOLATION` で fail-closed とする。

同意判定で使用するハッシュ契約は `consent.policy_segment_hash`（runtime 実効権限）と
`consent.provisioning_policy_hash`（provisioning 実効権限）の2系統に固定する。
`runtime_policy_hash` のような別名は本仕様では使用しない。

Consent Store のキー契約は以下に固定する。

- `scoped_id`
- `version`
- `target_label`
- `policy_segment_hash`
- `provisioning_policy_hash`

canonical hash の正規化規則（JCS準拠、配列順序、path canonicalization 境界）は
`EXECUTIONPLAN_CANONICALIZATION_SPEC.md` を正本とする。

---

## 5. 非交渉ルール（MUST）

1. Tier定義固定: Tier1 は `fail-closed`、Tier2 は明示 opt-in 必須。
2. canonical hash 固定: 同意対象は実効権限のみ（表示名等は除外）。
3. 非対話モード: 未同意なら即失敗。プロンプト禁止。
4. ポータビリティ定義: `os/arch/libc` など再現可能性レベルで判定。
5. Phase1完了条件: Deno/Wasm の権限逸脱ゼロを統合テストで証明。
6. Provisioning/Runtime の権限分離（ハッシュも分離）。
7. CAS は不変 + atomic 更新（壊れた中間状態を公開しない）。
8. Provisioning 通信先は許可レジストリに限定。許可外 redirect 拒否。
9. Lock 必須（Deno: frozen/cached-only/no-prompt、uv: locked/offline）。
10. 失敗は Ato 診断コードで返す（OS生エラーの丸出し禁止）。

---

## 6. 実行フェーズ

### 6.1 Phase 1: Provisioning

目的:

- toolchain 取得
- 依存解決
- CAS への格納

要件:

- 許可レジストリ以外へ通信不可
- lock/integrity 不成立時は停止
- Secret 注入禁止（BYOK を使わない）

### 6.2 Phase 2: Runtime

目的:

- ユーザーコード実行

要件:

- 同意済み `policy_segment_hash` のみ実行
- Tier1 は strict 実行 (`--no-prompt` 等)
- Tier2 は sandbox 可用性と pre-flight 条件を満たす場合のみ実行

---

## 7. 思考実験ケース（現時点）

### 7.1 コアケース

#### Case 1: 静的Web（HTML/CSS/JS）

入力例:

- `runtime=web`, `driver=browser_static`

期待動作:

- 依存解決なし
- 内蔵静的サーバーを `127.0.0.1` のランダムポートで起動
- GUIあり: ブラウザ起動を試行
- GUIなし: URL を stdout 出力して待機

防御ポイント:

- path traversal / symlink 逸脱を拒否
- `--open` 失敗は非致命フォールバック

#### Case 2: Deno フルスタック

入力例:

- `driver=deno`, `deno.lock` 同梱

期待動作:

- Provisioning: `deno cache --lock --frozen`
- Runtime: `deno run --cached-only --no-prompt ...`

防御ポイント:

- `deno.lock` 未同梱なら fail-closed
- `fs.read` 未宣言で static 配信失敗は「正しい失敗」
- Runtime 中の未キャッシュ動的 import は遮断される

#### Case 3: Python + uv（Tier2）

入力例:

- `driver=native`, `uv.lock` 同梱

期待動作:

- Provisioning: `uv sync --locked` で CAS 構築
- Runtime: nacelle 内で `uv run --offline`

防御ポイント:

- `~/.capsule/store` 全体マウントは禁止
- 必要 digest のみ RO auto-mount
- `/tmp` を tmpfs で提供
- `PYTHONDONTWRITEBYTECODE=1` 強制

#### Case 4: ネイティブELFバイナリ

入力例:

- `driver=native`, `entrypoint=["./bin/app"]`

期待動作:

- 起動前に互換性チェック
- 不適合なら `ATO_ERR_COMPAT_HARDWARE` で fail-closed

防御ポイント:

- `ldd` 直接実行は禁止
- ELF の `PT_INTERP`/`DT_NEEDED`/シンボルバージョンを静的解析
- CPU feature 要件も pre-flight で検証

### 7.2 追加ストレスケース

#### Case 5: 非対話CI

- 未同意権限要求時は即 `exit(1)`
- `--yes` は未同意昇格を許可しない

#### Case 6: オフライン初回実行

- 必要アーティファクト不足時は機械可読エラーを返す

#### Case 7: 同時実行レース

- `.tmp` へ展開 -> 検証 -> `rename(2)` atomic publish

#### Case 8: マルチユーザー

- `~/.capsule` は `0700`
- 他ユーザーからの cache poisoning を防止

#### Case 9: Linux機能差

- userns/landlock/bwrap 不可用時は降格せず fail-closed

#### Case 10: リダイレクト/レジストリ逸脱

- 許可外 host への redirect を拒否
- TLS/署名検証失敗で停止

#### Case 11: Secret漏洩

- ログ・エラー出力で secret を常時マスク
- `/proc` 覗き見対策を sandbox 側で強制

#### Case 12: TOCTOU / pathすり替え

- 事前 canonicalize + 実行時に解決済み対象へ拘束
- hash は正規化後パスに対して計算

### 7.3 追加ストレスケースへの回答（Fail-Closed ルール）

1. **Deno `npm:` 依存の lifecycle script**
   Runtime 中の依存展開に起因する任意コード実行経路を許可しない。検出時は `ATO_ERR_POLICY_VIOLATION` で fail-closed。
2. **uv の sdist フォールバック**
   Provisioning 中のソースビルド由来の任意コード実行を許可しない。解決不能時は `ATO_ERR_PROVISIONING_LOCK_INCOMPLETE` で fail-closed。
3. **CAS 破損（電源断/途中DL）**
   破損オブジェクトを可視化せず、検証済みオブジェクトのみ publish する。
4. **`ENOSPC` / inode 枯渇**
   partial object を公開せずロールバックし、`ATO_ERR_STORAGE_NO_SPACE` を返す。
5. **private registry + mTLS + 証明書ローテーション**
   trust 制約に一致しない証明書を拒否し、検証不整合時は `ATO_ERR_PROVISIONING_TLS_TRUST` で fail-closed。
6. **toolchain 同時更新競合（A/Bプロセス）**
   toolchain 更新は単一勝者の atomic publish のみ許可し、競合時は再検証後に再試行する。
7. **同一ユーザー悪意プロセスによる env 覗き見**
   user secret の env 直接注入を禁止し、同一ユーザーの `/proc` 覗き見耐性を満たす経路のみ許可する。
8. **lockfile 改ざん**
   Manifest と lockfile の canonical hash が一致しない場合は Provisioning 開始前に `ATO_ERR_LOCKFILE_TAMPERED` で fail-closed。

実装手段レベルの具体例（フラグ、syscall、実装パターン）は
`docs/implementation/EXECUTIONPLAN_HARDENING_GUIDE.md` を参照する。

---

## 8. 解決済みクリティカル防御仕様

1. **Secret 高信頼注入経路**
   user secret の env 直接注入を廃止し、`pipe(2)` または `memfd_create(2)` による無名 FD 経由受け渡しを標準とする。実行時 secret は `/proc` 由来の同一ユーザー覗き見耐性を前提要件とする。secret の分類（`user_secret` / `session_token`）と注入経路は `SECRET_CLASSIFICATION_SPEC.md` を正本とする。
2. **ELF 検証の固定**
   `goblin` 等による静的解析において `PT_INTERP`、`DT_NEEDED`、`DT_VERNEED`（`.gnu.version_r`）を必須解析対象とし、ホスト `glibc` バージョンと厳格照合する。
3. **Auto-mount 集合の決定性**
   mount-set は純粋関数 `f(lockfile) -> Deterministic_Mount_Set` で生成し、生成ロジック hash を `consent.policy_segment_hash` の入力に組み込む。
4. **Provisioning の供給網検証**
   `deno.lock` / `uv.lock` 内の SHA256 / SRI 検証を必須化し、不一致 artifact は即時破棄して fail-closed とする。

---

## 9. 次の実装マイルストーン

1. `ExecutionPlan` Rust 構造体 + canonical hash 実装。
2. Deno driver PoC（Provisioning/Runtime 分離、`--no-prompt`、`--cached-only`）。
3. Consent Store 接続（`scoped_id + version + target_label + policy_segment_hash + provisioning_policy_hash`）。
4. non-interactive deny-path の統合テスト整備。

---

## 10. I/O 契約境界

- **engine 内部I/O（CLI ↔ nacelle）**: stdin/stdout の JSON プロトコルを使用し、stderr は human logs を許可する。
- **ato 外部I/O（CLI ↔ 利用者/エージェント）**: Fail-Closed 診断は stderr JSONL で出力する。
- nacelle の stderr 表現は `ato-cli` が正規化し、外部契約としては `ATO_ERROR_CODES.md` を優先する。

---

## 11. Agentic Interface への分離

`--from-skill`、`Structured Failure for LLMs`、`ato-agent` など Agentic 連携仕様は
別仕様に分離予定とし、本書からは参照のみ行う。
