# ato-run/ato Lean バグチェック報告（対象コミット 8b058e5 / 2026-11-17）

再実行: `./run.sh`（未使用コード走査も含めるなら `RUN_CHECK=1 ./run.sh`）。同種の検証を続ける手順は `.claude/skills/lean-bugcheck/SKILL.md`。
対象ファイルは `dev` @ 4911151 でも未変更であることを確認済み。
（Lean カーネル検査 → Rust 再現 → 未使用コード走査を一括実行）

## 手法と範囲
- 小さく純粋なセキュリティ判定ロジック 6 箇所（約 1,500 行）を手書きで Lean 4 にモデル化（`AtoBugcheck.lean`、Mathlib 不使用、`sorry` なし、Lean 4.36.0 で検査済み）。
- 「成り立つべき性質」が証明できなかった箇所は反例を定理（`*_BUG`）として固定し、実コードで再現。
- 全体は Rust 約 40 万行。今回見たのはその一部で、ここに無い箇所は未検証。

## 見つかった問題
| # | 場所 | 内容 | Lean | 実コード再現 | 影響 |
|---|------|------|------|------|------|
| 1 | `extensions/providers/nacelle/src/system/path.rs` `validate_path` | 許可パスが**存在しない**と最も近い既存の祖先に丸められ、許可範囲が広がる。`/x/models` が無ければ `/` 全体が許可される | `PathAllow.widening_BUG` / 修正案 `fixed_sound` | 再現（`/etc/passwd` と `/root/.ssh/id_rsa` が通過） | 関数自体は現状どこからも呼ばれておらず nacelle もワークスペース外。潜在バグ |
| 2 | `services/netd/src/surface_websocket_auth.rs` `authorize_upgrade` | 消費済みグラント集合が 4096 件で満杯になると削除処理が無く、以後そのゲートウェイは正当な新規グラントも全て 500 で拒否 | `Grants.lockout_forever_BUG` | 再現（一時テストを追加して実行後に復元） | pixel / terminal ゲートウェイの恒久的な接続不能。正規の署名付きアサーションが 4096 回必要 |
| 3 | 同上 `on_request` | グラントを消費した**後**にサブプロトコルを検査するため、400 で拒否された接続でもグラントが使用済みになる | 未モデル化（コード読解のみ） | 未実行 | 軽微。#2 の枯渇も早める |
| 4 | `services/netd/src/egress/policy.rs` `TransferBudget::consume` | `fetch_add(bytes) + bytes` が u64 でラップすると予算超過でも `true` | `Budget.wrap_BUG`（非オーバーフロー時は `consume_sound` が成立） | 再現（release ビルド。debug では panic） | 1 回の読み取りは高々数十 KiB なので実質到達不能。`checked_add`/`saturating_add` で解消 |
| 5 | `extensions/materializers/static-web/src/manifest.rs` `validate` | `materialization_id` の文字種・長さ違反が `EmptyField` エラーで返る | 未モデル化 | 未実行 | エラー種別の誤りのみ |

## 証明できた性質（問題なし）
- egress ゲート: IPv4 射影 IPv6（`::ffff:a.b.c.d`）は必ず拒否され、許可された宛先が IPv4 ループバック / 169.254.0.0/16 に到達することはない（`Egress.mapped_always_denied`, `no_loopback_or_linklocal`）。当初ここを疑ったが `::ffff:0:0/96` が拒否リストにあり否定された。
- グラントの再利用は必ず拒否（`Grants.replay_rejected`）。
- capsulehub レート制限: バケット内カウントは上限を超えない（`RateLimit.count_le_limit`）。ただし固定窓なので窓の境界をまたぐと短時間に上限の 2 倍が通る（`boundary_burst`、設計上の性質）。

## 未使用コード
`cargo check --workspace --all-targets` は警告 0 件（コンパイラが検出できる未使用は無し。ただし `#[allow(dead_code)]` / `allow(unused*)` が 50 箇所ある）。以下は `dead-code/scan.py` による識別子ベースの推定で、マクロ経由・外部公開 API は誤検出しうる。

1. **コンパイルされないファイル（確認済み）** — `mod` 宣言が無い nacelle の 7 ファイル: `launcher/dev.rs`, `launcher/resolver.rs`, `manager/ebpf_enforcer.rs`, `manager/enforcement_guard.rs`, `manager/startup_gc.rs`, `manager/supervisor_mode.rs`, `tsnet.rs`。
2. **`extensions/providers/nacelle` 全体（Rust 約 2.2 万行）** — ワークスペースの `exclude` にあり、他クレートからの依存も CI ワークフローからの参照も無い。別リポジトリへ移行済みなら削除候補。
3. **存在しないディレクトリを指す `exclude`** — ルート `Cargo.toml` の `extensions/semantics/workspace` と `extensions/adapters/repository` は中身が 0 ファイル。
4. **どこからも参照されない `pub` 項目 63 件** — `dead-code/pub-never-referenced.txt`。例: `services/netd/src/net/client.rs` の `register_ephemeral_ingress` ほか 4 関数、`lib/ipc/src/computation.rs` の `ComputationCommand(Result)`、`lib/runtime-attempt/src/ephemeral.rs` の `launch_controlled`、`apps/cli/src/auth_device.rs` の `load_credential`、nacelle の `verify_license`・`download_*_runtime`。
5. **テストからのみ参照される `pub` 項目 105 件** — `dead-code/pub-test-only.txt`。
6. **未使用の可能性がある依存 24 件** — `dead-code/deps-maybe-unused.txt`。サードパーティ分（`thiserror`×2, `tracing`, `base64`, `data-encoding`, `serde_jcs`, `tempfile`, nacelle の `wasmtime` 系）は確度が高め。`apps/cli` / `apps/formation-worker` の内部 `ato-*` クレート 12 件は lib 名の違いや feature 連動の可能性があり要確認。`cargo-machete` はこの環境でビルドできず未使用。
7. **到達不能な分岐（Lean で証明）** — `manifest.rs` の `file.size > MAX_SAFE_JSON_INTEGER`（`MAX_FILE_SIZE` 検査に包含）と `checked_add(...).ok_or(ClosureLimit)`（オーバーフロー不能）。
8. **その他** — リポジトリ直下にコミットされた `.tmp-consolidated-pr.md` / `.tmp-d-pr.md` / `.tmp-e-pr.md`。`scripts/acceptance/**` の単体 `.rs` 7 本は Cargo 管理外（スクリプトから直接使うものなら問題なし、未確認）。

## 限界
- モデルは手書きで、実コードとの対応は読解に依存（シンボリックリンク・並行実行は捨象）。#1 #2 #4 は実コードで再現済み、#3 #5 は読解のみ。
- 仕様の強さを測る mutation testing は未実施（バグ版 / 修正版の対のみ）。
