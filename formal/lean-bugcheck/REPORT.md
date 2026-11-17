# ato-run/ato Lean バグチェック報告（2026-11-17）

## 主要機能の棚卸し（第1回、対象 c4f29fe5f）

優先度は影響範囲と調査順を示す。下記は機能ごとの網羅保証ではなく、現在確認した境界を明記するための一覧。API / PWA / Desktop は本 repo 外または別ビルド境界であり、今回は未検査。

| 優先 | 機能 | 今回の確認と状態 | 次に調べる範囲 |
|---|---|---|---|
| 高 | Run の記録とローカル保存 (`lib/objects`, `lib/local-execution`) | **新規の不整合を1件再現**。下記 B1。既存 object/repository/bundle 単体63件は成功 | 更新失敗・クラッシュ時の復旧、branch 作成の途中失敗、複数 Run の競合 |
| 高 | Portable Capsule v2/v3/v4 の受理・closure (`lib/objects`, `apps/portable-application`) | 形状、参照、digest、closure の入口を読解。object 側63件成功 | 全 profile での root と object kind の整合、未到達 object、外部 blob、サイズ、CLI/hosted の呼び出し境界 |
| 高 | Run admission と Contract receipt (`lib/runtime-attempt`) | 関連の既存 admission 9件と verification 15件成功。対象以外は未検査 | D/K 選択、権限・runtime 境界、全 observation と receipt の実行時結合 |
| 高 | Formation の source 取得・展開 (`lib/formation`, `apps/formation-worker`) | 展開の入口を読解。メモリ内圧縮展開に下記 U1 の要調査事項 | メモリ版とファイル版の入力経路、展開上限と deadline、symlink 連鎖、アーカイブ衝突 |
| 高 | Model Set の受信・キャッシュ (`lib/runtime-attempt/src/data_plane`) | range と digest 検証の一部を読解。形式モデル・追加再現は未実施 | 同時受信、resume、部分書込み、quota、取得先と grants の結合 |
| 高 | 通信の認可・egress (`services/netd`) | 前回の既知問題は修正済み。今回の新規モデルは未追加 | 並行予算、時計変動・再起動後の replay、DNS と接続の一貫性 |
| 中 | Semantic Core、Kernel、Composition (`lib/computation`, `lib/kernel`, `lib/compose`) | 未着手 | identity、Port、Evolution、配線・閉包の不変条件 |
| 中 | CLI lifecycle、Materializer、local Instance (`apps/cli`, `extensions/materializers`, `apps/portable-application`) | 一部の呼び出し元を読解したのみ | init/resume/stop、競合、保存・復旧、v2/v3 分離 |
| 中 | Coordinator、Formation Worker、Hosted Runner | 未着手 | Job/lease/attempt の帰属、取消・冪等性、実環境の権限境界 |
| 中 | ato-api、ato-pwa、Desktop、配信・外部連携 | 未着手 | 各 repo の契約・実装・認証・変更経路 |

### B1 — Record の開始位置を照合せず Run を進める（未修正）

`LocalCapsuleRepository::commit_observation` はトークン・`expected_head`・stream を検査するが、`record.head_before == expected_head` を検査しない。既存の正しい Record の後に別の `head_before` を持つ Record を渡すと受理して Run cursor を更新し、その後の `records_for_causal_branch` は `CausalHeadMismatch` になる。accepted RFC `LOCAL_CAPSULE_REPOSITORY.md` が示す Record の連続性と実装の不一致として扱う。

Lean: `RecordContinuity.mismatched_record_BUG`。実コード再現: `repro/record_continuity.rs` を `run.sh` から実行し、受理と履歴検証失敗を確認した。これは公開 repository API の不正入力での再現であり、通常の CLI の `RepositoryObservationSink::emit` は同じ locked head を両方に設定する。通常操作や外部利用者がこの不整合を誘発できることは未確認。プロセスクラッシュや実機の再開操作は未検証。

### U1 — メモリ内 source archive の展開量（要調査、未確定）

`lib/formation/src/source.rs` のメモリ内 `decompressed` は gzip/zstd を `Vec` 全体に展開してから `SourceLimits` による tar 検査を行う。圧縮サイズと展開サイズの比によって検査前のメモリ使用が大きくなり得る。`FileVerifiedArchive` の streaming reader には別の展開読み取り上限があるため、一律に全経路の問題とは言えない。実運用の `formation-worker/src/retained.rs` は pack した artifact をメモリ版で再測定するが、攻撃者が圧縮 bytes を直接制御できる経路、具体的な最大メモリ、実機での影響は未確認。Lean モデル・実コードの資源消費再現は未実施であり、確定バグには数えない。

## 引き継ぎ後の修正

対象は `feat/lean-bugcheck` @ `bbeea6f` を起点とする worktree。下の5件を修正し、Lean 4.36.0 で修正版のモデルを検査した。`run.sh` は失敗時に非ゼロ終了し、ソースを書き換えずに回帰検証する。Lean は `$LEAN` または `~/.elan/bin/lean` から指定する。

| # | 状態と検証 |
|---|---|
| 1 | 修正済み。存在しない許可ルートの残りのパスを保持し、`..` を含むルートを拒否。実コードの回帰と8件の path 単体テストを確認。シンボリックリンク変更の競合は未検証。 |
| 2 | 修正済み。有効期限内のグラントを保持し、期限切れを回収。観測した時刻の最大値を保持し、時計が戻っても期限切れグラントを再受理しない。期限内の4096件が埋まった場合は安全側で拒否し続ける。再利用防止は従来どおりゲートウェイのプロセス寿命内に限る。 |
| 3 | 修正済み。サブプロトコルの拒否をグラント消費前に行う。実コードの回帰テスト済み。 |
| 4 | 修正済み。加算を飽和させ、オーバーフローを明示的に拒否。release 相当の再現と netd テスト済み。並行実行の厳密な線形化は Lean では未モデル化。 |
| 5 | 修正済み。空文字と無効な識別子を別のエラーとして返す。Static Web のテスト済み。 |

検証: Lean カーネル検査、`formal/lean-bugcheck/run.sh`、netd ライブラリ129件、Static Web ライブラリ31件、nacelle path 8件。netd のローカルソケットを使うテストはサンドボックス外で実行した。ストレージ容量の数値比較 `StorageCapacity.admission_has_capacity` と境界値も追加で証明したが、容量の観測後に空き領域が変化しないことや予約は保証しない。

`cargo check --workspace --all-targets` と未使用コード走査を更新した。推定は未参照62件、テストのみ104件、未使用の可能性がある依存24件で、コンパイラ警告は0件。以下の説明と数値は元の走査時点の記録であり、現在の一覧は `dead-code/` を参照する。いずれも削除の根拠にはならない。全機能をモデル化しておらず、全バグの解消を意味しない。

## 引き継ぎ時の調査記録（対象コミット 8b058e5）

再実行: リポジトリ直下で `formal/lean-bugcheck/run.sh`（未使用コード走査も含めるなら `RUN_CHECK=1 formal/lean-bugcheck/run.sh`）。同種の検証を続ける手順は `.claude/skills/lean-bugcheck/SKILL.md`。
対象ファイルは `dev` @ 4911151 でも未変更であることを確認済み。
（Lean カーネル検査 → Rust 再現 → 未使用コード走査を一括実行）

## 手法と範囲
- 小さく純粋なセキュリティ判定ロジック 6 箇所（約 1,500 行）を手書きで Lean 4 にモデル化（`AtoBugcheck.lean`、Mathlib 不使用、`sorry` なし、Lean 4.36.0 で検査済み）。
- 「成り立つべき性質」が証明できなかった箇所は反例を定理（`*_BUG`）として固定し、実コードで再現。
- 全体は Rust 約 40 万行。今回見たのはその一部で、ここに無い箇所は未検証。

## 引き継ぎ時に見つかった問題（修正前の状態）
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

## 引き継ぎ時の未使用コード候補
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
- 修正後の再利用防止モデルは期限と既出フラグの判定を抽象化しており、HashMap の実装、同時実行、プロセス再起動、発行者の jti 一意性は証明していない。
- モデルは手書きで、実コードとの対応は読解に依存（シンボリックリンク・並行実行は捨象）。#1 #2 #4 は実コードで再現済み、#3 #5 は読解のみ。
- 仕様の強さを測る mutation testing は未実施（バグ版 / 修正版の対のみ）。
