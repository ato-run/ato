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
| 署名 ID | `security find-identity -v -p codesigning` = **Apple Development のみ**（Developer ID Application 無し）→ 公証不可 |

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
| UI-01 | RED | ato-pwa にローカル専用 My Apps 画面なし（既定は localStorage `ato_app_library`）。シェルは `__ATO_DESKTOP__` を注入せず `isDesktop()` が常に false → Desktop 専用 runner UI へ到達不能。Desktop ローカル保管 UI は staged PWA source_sha `756a13fc` で workspace に commit が無い | 未実装 + 所在不明 |
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

## 5c. Desktop / PWA 実測（ato-desktop `origin/dev` `2354137`, ato-pwa）

| 面 | 実測 |
|---|---|
| シェル→CLI | sidecar 3 本（`tauri.conf.json:19-31`）。解決は sibling 優先で release の PATH fallback 無し（`src/binary.rs:31-62`）。CLI argv は init/resume/stop/encap/run のみ（`host.rs:264-299`）、状態読みは `__desktop inspect`（`host.rs:374-390`）、同時 Run 1 本（`host.rs:94-160`） |
| 実行経路 | 同梱 PWA から `host::*` 6 コマンドは未使用。実体は loopback HTTP + `on_navigation` 横取り（`local_apps.rs:16-129`）→ 302 検証 → 別窓。Coordinator 死亡時は Runtime 停止だけで WebView へ通知しない（`lib.rs:529-553`） |
| ローカル一覧 | PWA にローカル専用画面なし。既定は localStorage、サーバ一覧は flag opt-in で `build:desktop` 未設定（`package.json:13`）。Instance 一覧/停止は `computeClient.ts:509,610` |
| open | PWA は `app_url` アンカー（`openApp.ts:29-33`）。Desktop ローカルは `http://127.0.0.1:PORT/v1/local-apps/<cinst>/` を横取り |
| 署名 | `tauri.conf.json:20 signingIdentity: "-"`（ad-hoc 固定）。`xtask` に notarize primitive はあるが資格情報が無いと skip（`main.rs:221-300`）。配布 0.1.7 は未公証・ad-hoc |
| CI/E2E | ato-desktop repo に CI 無し（workspace の `ato-desktop-bindings.yml` は旧構成で死んでいる）。`e2e/local/README.md` は "Status: empty"。tauri-driver v2 は **macOS 非対応** → 実機 WebView 自動化は不可 |
| Unknown | staged PWA ローカル保管 UI の内容、`/v1/local-apps` の実装所有者（api/PWA 双方に無し）、`__ATO_DESKTOP__` 非注入が意図か退行か |

UI-01 の含意: 「CLI が正本」の原則は既存構造と整合するが、Desktop 側は**同一 Instance を読む実装が repo に存在しない**。まず PWA の `build:desktop` にローカル一覧（`ato app list --json` 由来）を載せ、`__ATO_DESKTOP__` 注入の有無を決めるのが最小。`/v1/local-apps` の所有者を決めずに UI だけ作らない。
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

1. **署名/公証**: この Mac には **Apple Development 証明書しか無く Developer ID Application が無い**ため、現状では公証できず clean Mac の Gatekeeper gate を通せない。Developer ID 発行と notarytool 資格情報の用意が必要（ユーザー判断）
2. **公式 API**: 認証方式・quota/budget・送信同意のプロダクト判断（価格・データ送信）
3. **Kev の位置づけ**: P0 gate に含めるか、optional demonstration に留めるか（MOD-01 の PASS 定義が変わる）
3b. **API の失敗契約**: 409 語彙のまま CLI で写像する（推奨・互換）か、429/402 + Retry-After を versioned endpoint で新設するか
4. **実機 E2E の実行場所**: この sandbox では `ps` 不可。Terminal.app 実行を許容するか、exec 制限のない環境を用意するか

## 9. 実測ログ（2026-11-17・macOS 15.8.1 / M1 / 16GiB）

この節は §2 の RED 一覧に対する**その後の実測**であり、判定が変わったものだけを上書きする。

### 9.1 GitHub URL → ローカルアプリ（新規に GREEN）

```sh
ato source fetch https://github.com/octocat/Spoon-Knife --out <dir>
# commit d0dd1f61b33d64e29d8bc1372a94ef6a2fee76a9
# tree   sha256:5b70fb337ee6f9e170f0abd195f08e13ec7ace5831d8dcdcb0ecc51945ea77d1  3 files / 1391 bytes

ato form <dir>/source --runtime local --network denied --out <dir>/out
# seal / runtime_verification / cleanup / publication = succeeded, fully_satisfied: true
# attempt-record: K sha256:872f2b8e…, D sha256:c1bbb704…, outcome "verified"

ato app add https://github.com/octocat/Spoon-Knife
# instance linst_95805a78…, K sha256:2132c2ee…, D sha256:26facb1d…, bundle sha256:26ab765a…

ato run <ATO_HOME>/sources/app-*/application.capsule --no-open --verification-receipt r.json
# fully_satisfied: true / GET / 200 / body_sha256 2b391997… / target cli-local
```

同じ commit を `--reference` で再取得すると **tree digest と archive sha256 が一致**した（同じ入力→同じソース）。

### 9.2 追加された CLI 面（いずれも実機で実行済み）

| コマンド | 状態 | 実測 |
|---|---|---|
| `ato doctor [--json]` | 実装 | `ato.doctor/1`。この sandbox では `process-identity` が fail（`ps` 不可）を正しく申告して exit 1 |
| `ato app list [--json]` | 実装 | `ato.local-instance-list/1`、state は `active_run` から導出 |
| `ato app remove [--purge-data]` | 実装 | 既定はデータ保持（`<ATO_HOME>/removed/<id>/state`）、purge のみ削除 |
| `ato app open` | 実装 | 停止中は型付き理由で拒否 |
| `ato app add <file \| Hub ref \| GitHub URL>` | 実装 | file/Hub は同一解決器、GitHub URL は §9.1 |
| `ato source fetch` | 実装 | commit 固定・上限・既存 verified-archive authority で展開 |

### 9.3 判定が変わっていない RED（理由付き）

| ID | 現状 | 理由 |
|---|---|---|
| MAC-01 | 一部 | doctor は実装。Homebrew は **tap の信頼が必要**（`brew info ato-run/ato/ato-cli` → "Refusing to load formula … from untrusted tap"）。配布物は ad-hoc 署名・未公証 |
| CAP-01 / APP-01 | 一部 | list/remove/open は実装・テスト済み。start/stop/restart は sandbox が `/bin/ps` を拒否するため未判定（製品不具合ではない） |
| CAP-02 | 一部 | Hub 参照の解決器は動作（`hub-not-found` を実 API から取得）。`capsules/ato/hello` は存在しない |
| SRC-02/03 | 未 | LLM 生成 D は公式 API の認証・同意・予算の判断待ち。409 を 429 に変えるかの契約判断も未 |
| MOD-01/02 | 未 | `ato model install` 未実装。Kev の head.pt は provenance と SHA 不一致 |
| UI-01 | 未 | PWA/Desktop にローカル専用画面なし。`__ATO_DESKTOP__` 非注入 |
| SEC-01 | 一部 | GitHub 取込は既存 verified-archive の containment に依存。悪性 fixture は未追加 |
| REL-01 | 未 | 署名・公証・再現ハーネス未 |

### 9.4 レビュー用 PR

- PR-00 監査: #1871
- PR-02 `app list/open/remove` + doctor: #1876
- PR-03 `app add`（file/Hub）: #1877
- PR-04 `source fetch`: #1878
- PR-05 `app add <github-url>`（静的サイト）: #1880

すべて Draft。CI の macOS `cli` は pass、ubuntu/windows/workspace の赤は dev 既存赤（`ato-adapter-pty` の Windows 非対応、`workspace_control`/`derivation_selection`）。


### 9.5 PR の合成確認（2026-11-17）

`test/selfhost-integration`（`455b9df93`）で #1876 / #1878 / #1880 をマージした。衝突は `apps/cli/src/lib.rs` の2箇所（`Commands` enum と dispatch）のみで、両側のコマンドを残して解消。統合バイナリで以下が通ることを確認した。

| # | 操作 | 結果 |
|---|---|---|
| 1 | `ato doctor` | target/ato-home/docker pass、process-identity fail（`ps` 不可を正しく申告） |
| 2 | `ato app add https://github.com/octocat/Spoon-Knife` | commit `d0dd1f61…` / tree `5b70fb33…` / Instance 生成 |
| 3 | `ato app list` | `stopped` / K `2132c2ee…` |
| 4 | `ato app add samples/interop-static.capsule` | 2件目 |
| 5 | `ato app list --json` | `ato.local-instance-list/1` に2件 |
| 6 | `ato app remove <id>` | データは `removed/<id>/` に保持 |
| 7 | `ato app list` | 1件 |
| 8 | `ato run <取込済み app>` | `fully_satisfied: true` |
| 9 | `ato app start <id>` | `process-identity-unavailable`（sandbox 制約を明示） |
| 10 | `ato app add https://github.com/ato-run/ato` | `unsupported-source`（静的 entry 無し。実行を装わない） |

### 9.6 Homebrew の実測（PR-01 の前提）

```
$ brew info ato-run/ato/ato-cli
Error: Refusing to load formula ato-run/ato/ato-cli from untrusted tap ato-run/ato.
Run `brew trust --formula ato-run/ato/ato-cli` or `brew trust ato-run/ato` to trust it.
```

formula 自体は実在し v0.9.3 の arm64 sha256 がリリース tarball と一致するが、**Homebrew は第三者 tap を信頼しないと読み込まない**。公開手順・初回案内には `brew trust ato-run/ato` を明記する必要がある（システム変更を伴うため実機インストールは未実施）。


### 9.7 Hub の実態（CAP-02 の前提）

```
$ curl -s https://api.ato.run/v1/hub/capsules
{"items":[],"total":0}
```

公開 Capsule Hub は**現在0件**。したがって `capsules/ato/hello` が 404 なのは個別の欠落ではなく、参照先そのものが空である。CAP-02 の Hub 経路（取り込み成功 E2E）は、誰かが公開するまで実データで検証できない。初期ユーザーの価値は、ローカル `.capsule` と GitHub URL の2経路で成立させるのが現実的。


### 9.8 CAP-01 / APP-01 の判定（libproc 統合後・2026-11-17）

**方針変更（ユーザー決定）**: `libproc` 等の安全な Rust クレートの追加を許可。`ato-local-execution` の `#![forbid(unsafe_code)]` は維持し、`/bin/ps` 依存を廃止。PID だけでなく開始時刻を記録して PID 再利用による誤停止を防ぐ。

実装:
- `lib/local-execution/Cargo.toml`: macOS 限定で `libproc = "0.14"`（crate の Cargo.toml で license MIT を確認）
- `process_start_time`: `libproc::proc_pid::pidinfo::<libproc::libproc::bsd_info::BSDInfo>` の `pbi_start_tvsec/pbi_start_tvusec` を `{sec}.{usec}` で安定化
- `current_process_group`: 同じ `BSDInfo.pbi_pgid`
- `boot_session_identity` は `sysctl -n kern.boottime`（`ps` ではない）

実測（macOS arm64, `ATO_HOME` は作業ツリー内）:

```
app import                   -> linst_263bef44…
app start <id> --no-open     -> Run lrun_7b25ff02…, URL http://127.0.0.1:49986
app inspect <id>             -> state active, pid 26896, url 127.0.0.1:49986
curl <endpoint>/             -> http=200
app stop <id>                -> Stopped Run lrun_7b25ff02…
app start <id> --no-open     -> 新しい Run lrun_d597fbb2…, URL http://127.0.0.1:50081（再起動）
app stop <id>                -> Stopped
app inspect <id>             -> active_run: null
```

既存回帰 `cargo test -p ato-cli --test portable_application`: **13 passed / 5 failed**（以前は 10/8）。
残る5件はすべて **macOS process sandbox の初期化が `Operation not permitted`** で落ちるもので、`~/.ato/cache` への書き込みと `sandbox_init` がこの実行環境で禁止されているため。`ps` に依存していた3件（`a_static_run_is_owned_by_its_run_until_stopped`, `imported_local_instance_can_stop_and_restart_without_reimporting`, `snapshot_bundle_start_records_the_installed_snapshot_in_its_receipt`）は **PASS に転じた**。

未検証: クラッシュ後の孤児回収、macOS 再起動をまたぐ状態復元（実行環境の再起動が必要）。

### 9.9 REL-01 の完了条件変更（ユーザー決定）

- 初期公開は **macOS arm64 CLI**。Homebrew Formula が正式な配布経路。
- **Developer ID 署名・Apple 公証は初期公開の必須条件から外す**（Tauri の `.app` 直接配布は後続タスク）。
- クリーンな Mac で **OS のセキュリティ設定を変更せずに** インストール・実行できることを実測する。
- SHA256 検証・リリースの出所確認・アンインストールは受入条件として残す。
- `xattr` による隔離属性の強制削除や Gatekeeper の無効化を手順に含めない。
- 管理 UI は `ato ui`（既存 PWA 画面の再利用）を将来の入口として検討し、Tauri 配布を待たない。

### 9.10 MAC-01: 配布物そのものの実行確認（2026-11-17）

配布されている v0.9.3 arm64 の tarball（GitHub Release asset、sha256 `6f808611…b9079`）を展開し、workspace 内でそのバイナリを直接実行した。

```
$ .tmp/pr00/ato-cli-aarch64-apple-darwin/ato --version
ato 0.9.3

$ codesign -dv --verbose=2 <binary>
Signature=adhoc (linker-signed), TeamIdentifier=not set
$ spctl -a -vv <binary>
rejected

$ ato run samples/interop-static.capsule --no-open --verification-receipt r.json
Bundle: sha256:95b837f4…bdd813 / K: sha256:b565e9d7…b741db
fully_satisfied: true
```

含意:

- **未公証（adhoc）の配布物でも、既製 Capsule の実行と Receipt 発行は成立する**。Homebrew Formula 配布に公証が必須でない、という方針を実測で裏付けた。
- ただしこの検証は「展開済みファイルの実行」であり、**quarantine 属性が付いたダウンロード経路（ブラウザ経由）は対象外**。Homebrew は quarantine を付けないため Formula 経路は問題にならない。`xattr` 削除や Gatekeeper 無効化は行っていない。
- `brew install` 自体はこの DSH サンドボックスでは実行できない（`~/.homebrew/trust.json.lock` への書き込み拒否）。手順は `macos-selfhost-install-check.md` に分離した。
- `ato doctor` は本セッションの PR（#1876）で追加されたため、v0.9.3 には含まれない。次回リリースで配布物に載る。

### 9.11 セッション引き継ぎ（2026-11-17 時点）

実装済み（すべて Draft PR・本番デプロイなし）:

| PR | ブランチ | 内容 | 実測 |
|---|---|---|---|
| #1871 | docs/macos-selfhost-pr00 | 監査・実測ログ・インストール runbook | §9.1〜9.10 |
| #1876 | feat/cli-app-lifecycle | app list/open/remove + doctor + libproc + `ato ui` | 6/6 + 2 テスト、実機 lifecycle |
| #1877 | feat/app-add-hub | app add（file / Hub 参照） | 3/3 |
| #1878 | feat/source-fetch | source fetch（commit 固定 + provenance） | 6/6 + 実 GitHub |
| #1880 | feat/app-add-github | app add <github-url>（静的サイト） | E2E `fully_satisfied: true` |
| (検証) | test/selfhost-integration | 4 PR の合成確認 | 9 tests pass |

実機で確定した事実:
1. durable Instance の start/stop/再起動は `libproc` 統合後に動作（`ps` 非依存）。
2. 配布 v0.9.3（adhoc・未公証）バイナリでも既製 Capsule の実行と Receipt は成立。
3. GitHub URL → commit 固定 → Formation（既知 static D）→ 実行 → `fully_satisfied: true`。
4. `ato ui` で一覧・起動・停止・open・登録解除（API）が動作。

次に必要な作業（コード変更・要レビュー）:
- `ato ui` 画面への open/remove ボタン追加
- Kev: `kev` provider の identity 分離と `ATO_KEV_*`、head.pt attestation 要求
- LLM 生成 D: 401/403/409/422/429、`unsatisfied`/`exhausted` の非 409 表現、送信同意
- Hub staging 登録（`capsules/ato/hello`）
- 公開ゲート（署名・公証・production は最終承認待ち）

注意: このセッションの実行環境では `brew install` と macOS process sandbox が使えない。前者は Terminal 用 runbook、後者は既知の環境要因として記録済み。
