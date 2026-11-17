# macOS セルフホスト P0 — PR-00 現状監査と受入契約

Status: Current（2026-11-17 時点の実測記録。ロードマップの追跡正本は GitHub Issues/Project であり、この文書は監査証跡と受入IDの契約のみを持つ）

対象: ato (Rust CLI/Runtime/Formation), ato-api, ato-desktop, ato-pwa, Kev-0.8B
基準: `origin/dev` = `a0d1e9c22`（worktree `apps/ato/.worktrees/macos-selfhost-pr00`）

## 0. 実行環境（実測）

| 項目 | 値 |
|---|---|
| OS | macOS 15.8.1 (24H32), Darwin 24.6.0 |
| CPU / RAM | Apple M1 (arm64) / 16 GiB |
| ato バイナリ | `apps/ato/target/debug/ato` = 0.9.3, commit 32e3b4cb…, dirty（local nightly 由来） |
| ローカル ato checkout | branch `nightly` @ `65cec62b7`、`origin/dev` と 6875 ahead / 7247 behind → **作業基準にしない** |
| ローカル ato-api checkout | branch `codex/public-install-invites` @ `e7b39a98`、`origin/dev`(`50cf3929`) より **1718 behind**（local router 254行 vs dev 1319行）→ 監査は dev 基準 |
| 配布版 | GitHub Release v0.9.3 (2026-11-16) |
| DSH sandbox | full enforcement。`/bin/ps` 実行不可（後述 §6） |

## 1. 実測で GREEN のもの（macOS arm64・今日のコード）

G1. **ephemeral Run は動く**（K/D/Receipt が実物で揃う）

```sh
cd apps/ato && ATO_HOME=$PWD/.tmp/pr00/home \
  ./target/debug/ato run samples/interop-static.capsule --no-open \
  --verification-receipt .tmp/pr00/out/receipt.json
```

- bundle `sha256:95b837f4…bdd813`, K `sha256:b565e9d7…b741db`, D `sha256:87a0ac84…b7a3cb`
- endpoint `http://127.0.0.1:<port>`、receipt `fully_satisfied: true`、observations: `app-proof`(GET /proof.txt 200, body_sha256 5ec8f5a4…) + `source-identity`(workspace digest 923c3f56…) がともに `satisfied`
- receipt schema `ato.contract-verification-receipt/1`, target `cli-local`

G2. **durable Instance の登録は動く**: `ato app import samples/interop-static.capsule` → `ato.local-instance/1`（`instance_id=linst_95d81fb0…`、`application_id=lapp_3ed462f6…`、`selected_derivation_ref` 明示）を出力。

G3. **既存 CLI 面**（`target/debug/ato --help` 実測）: `run / pack / form / form-input / form-session / form-verify / runtime-network / hub {push,pull,publish} / data / workspace / model-set {import} / app {import,start,stop,inspect,export} / login / upload / version --json`。

G4. **Formation の決定プロバイダ抽象は既にある**: `ato form --decision-provider jev`（`--max-decisions`, `--decision-timeout-ms`）。Kev はこの contract の別実装として載せるのが最短。

G5. **Homebrew tap は実在**: `ato-run/homebrew-ato`（`Formula/ato-cli.rb`, `cli.rb`, `nacelle.rb`）。v0.9.3 formula が arm64 tarball と sha256 `6f808611…b9079` を pin。→ 導入経路は「無い」のではなく「名前とゲートが違う」。

G6. **GitHub ソース取得の既存実装はサーバ側にある**: `apps/formation-worker/src/job/source_fetch.rs`。`redirect none`・圧縮無効・deadline・最大バイト数・`codeload.github.com/{owner}/{repo}/tar.gz/{commit}` 固定で、`resolved_commit_sha`(40桁 hex) を必須化。テスト `github_url_is_public_pinned_and_cannot_inherit_control_plane_credentials`。**CLI からは未露出**。

G7. **Kev-0.8B は実在し、arm64/MLX 対応**（`.tmp/pr00/kev-audit.md`）: adapter 43,338,624 B / head.pt 2,103,999 B / tokenizer 19,989,325 B / base Qwen3.5-0.8B-Base 1,746,942,600 B、合計 1.836 GB、全層 Apache-2.0、`POST /v1/systemone` が Ato の Choice 形状と一致。

## 2. 実測で RED（受入ID別）

| ID | 判定 | 実測根拠 | 分類 |
|---|---|---|---|
| MAC-01 | RED | `ato doctor` 不在（`--help` に無し、`DoctorArgs` は connected-realization-worker 限定）。配布バイナリは `Signature=adhoc` / `spctl -a` **rejected** / TeamIdentifier 無し | 未実装 + 配布ゲート |
| CAP-01 | RED（環境遮断） | `ato app start linst_…` → `error: local Instance worker exited before becoming active`、worker log = `worker process start time is unavailable`（原因は §6）。`app list` 不在 | 環境遮断 + 未実装 |
| CAP-02 | RED | Hub `capsules/ato/hello` → `hub-not-found … (not_found)`（api.ato.run）。`ato app import` は拡張子 `.capsule` のローカルファイル限定（`accepts only a portable .capsule file`） | 未実装 + 参照欠落 |
| SRC-01 | RED | `ato form <dir>` のみ。GitHub URL を受ける CLI 経路なし（G6 はサーバ側） | 未実装 |
| SRC-02 | RED | LLM 候補生成の CLI 経路は `form-session` と `form --runtime-network` のみ。provider は `jev` 固定・鍵必須。API 側は**未認証の Formation/generation 経路が 1 本も無い**（全経路 401） | 未実装 |
| SRC-03 | RED | 予算/期限の拒否は **409**（`budget_mismatch`/`deadline`/`exhausted`）で 429/402/Retry-After は無し。per-account 検索上限も無し。CLI 側の統一失敗 UX と `--json` 契約も無い | 未実装 |
| MOD-01 | RED | `ato model install/status` 不在（`model-set import` は既存ディレクトリ取込のみ）。Kev→Ato 接続は `JEV_BASE_URL` 定数固定で不可、`model` 接頭辞 `jev-` 必須 | 未実装 |
| MOD-02 | RED | モデル未導入時の fallback を検証する E2E/fixture が無い | 未実装 |
| APP-01 | RED | `app list / open / remove` 不在。start は §6 で検証不能 | 未実装 + 環境遮断 |
| UI-01 | RED（未監査完了） | ato-desktop/ato-pwa のローカルライブラリ実装は別途 `.tmp/pr00/desktop-audit.md` | 未実装 |
| SEC-01 | RED | GitHub archive の quarantine はサーバ側テストのみ。CLI 側の悪性 archive fixture が無い | 未実装 |
| REL-01 | RED | 署名/公証/再現ハーネス無し。`origin/dev` の `dist-workspace.toml` / `.github/workflows/release.yml` に signing/notarization 設定が無い（grep 0件） | 未実装 |

## 3. 文書とコードの不一致（Detected mismatch・勝手に直さない）

| # | 計画の記述 | 実測 |
|---|---|---|
| M1 | `brew install ato-run/ato/ato` | tap の formula は `ato-cli`（`brew install ato-run/ato/ato-cli`）。`ato.rb` は存在しない |
| M2 | ato-desktop = Tauri | 正しい（`ato-desktop/Cargo.toml`: `ato-desktop-tauri`）。なおローカル `nightly` checkout の `AGENTS.md` は「GPUI + Wry, NOT Tauri」のまま stale（`origin/dev` では「Desktop は別 repo」へ修正済み）→ 作業基準を `dev` に置く理由 |
| M3 | 「各repoの現行 AGENTS.md」 | AGENTS.md があるのは `apps/ato` のみ。ato-api / ato-desktop / ato-pwa には無い |
| M4 | 既存 Formation 修復探索 PR #1452（未マージ前提） | 2026-10-29 に **MERGED** |
| M5 | `ato model install kev-0.8b` / `model status` | `model-set import`（ディレクトリ取込）だけが実在。install/status は新設 |
| M6 | `ato app add` が Hub ref を受ける | `app import` はローカル `.capsule` 限定。Hub ref は `ato run` だけが解決 |
| M7 | `capsules/ato/hello` が既製 Capsule として存在 | api.ato.run に **存在しない**（404） |
| M8 | Kev の資産同一性は provenance で検証できる | provenance の `head_sha256`(bc77488a…) と公開 `head.pt`(f400bd12…) が不一致（adapter は一致） |

## 4. Fixture の現状

| 必要 fixture | 状態 |
|---|---|
| ローカル hello `.capsule` | 代替あり: `samples/interop-static.capsule`（K 付き static web） |
| Hub `capsules/ato/hello` | **無い**（404）。PR-03 で publish するか fixture を差し替える判断が必要 |
| 小型静的 Web OSS | `samples/interop-static` で代替可 |
| 最小 HTTP API OSS | `samples/interop-datasette` は実在だが重い（Python + Datasette）。P0 用に軽量 fixture を別途固定する |
| 不正 archive / K 不一致 / API 失敗 / モデル未導入 | **無い**。PR-04/05/06 で追加 |

## 5. `--json` 契約（PR-00 で固定する範囲）

既存を再利用し、新しい schema を増やさない:

- Instance 1件: 既存 `ato.local-instance/1`（`instance_id`, `application_ref`, `contract_ref`, `bundle_sha256`, `available_derivation_refs`, `selected_derivation_ref`, `data_snapshot_ref`, `bindings`, `created_at`）
- 一覧: `ato.local-instance-list/1` = `{ schema, instances: [ato.local-instance/1], runs?: [...] }`（新設）
- 状態: `imported | starting | running | stopped | failed`。既存 `active_run`（`run_id`, `pid`, `endpoint`, `process_start_time`, `boot_session`）から導出し、二重の真実を作らない
- 診断: `ato.doctor/1`（新設） = `{ schema, os, arch, checks:[{id,status:pass|warn|fail,detail}], supported:{targets, unsupported:[...]} }`
- 失敗は exit code + `{ "error": { "code", "message", "retryable" } }`。`hub-not-found` / `state-migration-approval-required` 等の既存コード語彙を流用

CLI 名は `app add|list|remove|open` を新設し、`app import` は互換エイリアスとして残す（意味論は変えない）。

## 5b. Formation API（ato-api）実測契約

基準: `origin/dev` `50cf3929`（runtime-network ツリーは stg/main と同一）。詳細は `.tmp/pr00/api-audit.md`。

| 面 | 実測 |
|---|---|
| マウント | `src/routes/runtime_network.ts`（1319 行）を `index.ts:996` |
| runner 側 | `PUT /runtimes/self`, `POST /runtimes/self/availability`, `POST /attempts/claim`（204 か ticket、`operation_id` で再送冪等）, `GET /attempts/:id/source`, `POST /attempts/:id/result` |
| requester 側 | `POST /satisfy`（48 MiB）, `GET /satisfy/:id`, `GET /verified-routes`, `GET /runtimes`, `POST /sources{,...}`, `POST /sources/from-formation`, `hosted-exploration/{preview,confirm,cancel,promotion}` |
| search | **`/searches` は存在しない**。opaque `search_id` を `/satisfy/:id` と `/exploration/:searchId/*` で読む |
| 認証 | bearer `ato_rnr_` runner token か session。runner 登録は session 必須。`optionalAuth("apps:read|write")` は OAuth token のみに作用し runner token は scope を迂回 |
| 制限 | per-search budget（`protocol.ts:189-238`）。拒否は 409。429/402/Retry-After は 0 件、per-account 上限も無し。coordinator timeout 30 分、claim は非ブロッキング |
| 同意 | hosted exploration は preview → confirm で `sha256(frozen_json)` を束縛（`hosted_exploration_consent.ts:40-90`）。ただし staging の feature flag 配下のみ |
| テスト | service-level 108 ブロック（HTTP なし）。HTTP テストがあるのは owner/promotion routes と formation-source-runtime-intake のみ。**claim/source/result は HTTP 未テスト** |
| 運用証跡 | branch protection なし（private/free）、environment 0、直近 stg Deploy 15/15 は GitHub Actions 課金アノテーションで job 未起動。`/health` は version のみ |

P0 への含意:
- 「アカウント無しで手元 .capsule の import/run 管理」は **API 非依存のローカル経路だけ**で成立させる（現状の `ato run` / `app import` は API 不要で動く）。
- 公式 API 既定の Formation は account 必須のままでよいが、**429/402 を新設するか 409 語彙のまま CLI で写像するか**は契約判断（既存 consumer を壊さないため、まず CLI 側で `retryable=false`/`reason` へ写像するのを推奨）。
- rate limit / quota / 監査は PR-05 の前提として未実装。公開 API にするなら別 PR で versioned endpoint + 制限を足す。
- staging が課金でデプロイできていないため「stg で検証済み」は現時点で主張できない（ローカル実測のみ）。

## 6. 環境遮断（この DSH セッション固有・製品不具合と混同しない）

- `/bin/ps` の実行が拒否される（`Operation not permitted`, exit 126）。workspace にコピーした `ps` も SIGKILL。
- 既存コードは macOS のプロセス同一性を `ps -o lstart=` / `ps -o pgid=` / `sysctl kern.boottime` に依存（`lib/local-execution/src/process.rs:98-154`）。`sysctl` は通るが `ps` が通らないため `ato app start` は必ず失敗する。
- したがって **CAP-01 / APP-01 はこのセッションでは判定不能**。同じコマンドを通常の Terminal.app で実行して判定する。ここで「RED=製品欠陥」と断定しない。
- なお、この依存自体は「制限された exec 環境では durable Instance が起動できない」という実装上の脆さでもある（PR-02 で sysctl/libproc ベースへ置換するか、明示的な診断エラーにするかを決める）。

## 7. 次の作業順と前提

1. PR-01: 署名/公証の判断（Developer ID + notarytool 資格情報 = **ユーザー判断**）、`ato doctor --json`、Intel/Linux 非対応表示、clean Mac 実機 gate
2. PR-02: `app list/open/remove` + 状態遷移 + 孤児回収。**実機 gate は Terminal.app で実行**
3. PR-03: `app add`（file/Hub 統一）、`capsules/ato/hello` を publish するか fixture 差し替えを決定
4. PR-04: GitHub URL 取込。`formation-worker` の `source_fetch` を共有 crate へ抽出して再利用（再実装禁止）
5. PR-05: `ato form`/`runtime-network` への接続、同意 UX、予算・失敗契約
6. PR-06: `kev` provider 追加（provider 識別/base URL/model の env 分離、`options` を criteria へ、head.pt attestation）
7. PR-07/08: Desktop ローカルライブラリ、横断 E2E、配布

## 8. 承認が必要な事項（止めずに進められる範囲の境界）

1. **署名/公証**: Apple Developer ID 証明書と notarization 資格情報の有無（無ければ clean Mac の Gatekeeper gate は通せない）
2. **公式 API**: 認証方式・quota/budget・送信同意のプロダクト判断（価格・データ送信）
3. **Kev の位置づけ**: P0 gate に含めるか、optional demonstration に留めるか（MOD-01 の PASS 定義が変わる）
3b. **API の失敗契約**: 409 語彙のまま CLI で写像する（推奨・互換）か、429/402 + Retry-After を versioned endpoint で新設するか
4. **実機 E2E の実行場所**: この sandbox では `ps` 不可。Terminal.app 実行を許容するか、exec 制限のない環境を用意するか
