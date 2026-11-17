# macOS self-host release gate — 2026-11-18

Status: Current (measured) / Draft (pending decisions)

この文書は macOS self-host リリースの公開ゲート判定を、**実測値と阻害要因**で
1か所にまとめたものである。受入条件は指示書の5項目。PASS は「実機で動作し、
Verifier/Receipt を得た」場合だけ記す。未測定・環境要因・承認待ちは明示する。

## 判定サマリ

| # | 機能 | 判定 | 根拠 / 阻害要因 |
|---|---|---|---|
| 1 | CLIインストール（Homebrew） | **BLOCKED（ユーザー操作）** | 手順書 `docs/ops/macos-selfhost-install-check.md` は完成。`brew install ato-run/ato/ato-cli` はサンドボックスが `~/.homebrew` への書き込みを拒否するため、ユーザーの Terminal 実行が必要 |
| 2 | Kev-0.8B（任意DL + ローカル推論） | **PASS** | 12 asset / 1,835,330,682 bytes を sha256 固定で導入し、実推論を実測。再現テスト `apps/cli/tests/model_inference.rs` |
| 3 | Capsule取り込み（.capsule + Hub参照） | **PASS** | `.capsule` の durable lifecycle は PR #1876 で実測。Hub参照→install→起動→停止→再起動を dev で実測（§3b）。Hub参照→検証→実行の receipt も `fully_satisfied: true`（§3） |
| 4 | ソースから実行（GitHub → Formation） | **PARTIAL** | 既知D: **PASS**（Spoon-Knife を v0.9.5 で form → receipt `fully_satisfied: true`、§4）。hosted Runtime Network は Runtime 未登録で候補0（§4b）。LLM生成DはローカルLLMで typed Draft 生成まで実証（PR #1908/#1909）、検証・実行は Runtime 登録待ち |
| 5 | ローカル管理（CLI + Desktop UI） | **PARTIAL** | CLI と `ato ui` は PR #1876 で実測（UI から起動→配信→停止、§5）。Desktop（Tauri, 別repo）は未着手 |

## 1. リリース成果物（Homebrew が入れる binary）の実測

Homebrew Formula は release.yml（cargo-dist）が公開する tarball を入れる。
その binary と同一のビルドを手元で作り、**開発ツール無しで既製 Capsule が動く**ことを確認した。

```
$ cargo build --release -p ato-cli
$ ./target/release/ato --version
ato 0.9.4-stg

$ otool -L target/release/ato
/System/Library/Frameworks/SystemConfiguration.framework/...
/System/Library/Frameworks/CoreFoundation.framework/...
/usr/lib/libiconv.2.dylib
/usr/lib/libSystem.B.dylib
```
→ 依存は **macOS 同梱の system library のみ**（開発ツールや Homebrew の dylib を要求しない）。

```
$ ATO_HOME=<fresh> ./target/release/ato run samples/interop-static.capsule --no-open --verification-receipt r.json
Capsule: sha256:b565e9d7…b741db
Route:   sha256:87a0ac84…b7a3cb
Runtime: static web
URL: http://127.0.0.1:55133
receipt: fully_satisfied True | contract sha256:b565e9d7…
```

```
$ ATO_HOME=<fresh> ./target/release/ato model status --json
model status: installed False assets 12 total_bytes 1835330682
```
→ 未導入でも 12 asset の pin 表を返し exit 0（初回ユーザーの最初のコマンドが失敗しない）。

binary size 15 MB。**残る未測定は brew install 自体のみ**（サンドボックスが
~/.homebrew への書き込みを拒否するため、ユーザーの Terminal 実行が必要）。

### 公開済み v0.9.5 成果物の検証（Homebrew が実際に入れるバイト列）

tap の Formula（`ato-run/homebrew-ato/Formula/ato-cli.rb`）が指す arm64 tarball を取得し、
**Formula の sha256 と一致**することを確認した上で、その中のバイナリを直接実行した。

```
$ curl -L -o ato-cli.tar.xz https://github.com/ato-run/ato/releases/download/v0.9.5/ato-cli-aarch64-apple-darwin.tar.xz
$ shasum -a 256 ato-cli.tar.xz
b28c7adb635aa7104a792fc7ffae95132be25e865a5faf1ad971085db8a1b82b
$ grep sha256 Formula/ato-cli.rb   # arm64-apple-darwin
sha256 "b28c7adb635aa7104a792fc7ffae95132be25e865a5faf1ad971085db8a1b82b"   ← 一致

$ ./ato --version
ato 0.9.5
$ codesign -dv ./ato
Signature=adhoc / TeamIdentifier=not set
$ otool -L ./ato | grep -v -E '^/System/|^/usr/lib/'
（出力なし = system library のみ）

$ ATO_HOME=<fresh> ./ato run samples/interop-static.capsule --no-open --verification-receipt r.json
Runtime: static web / URL: http://127.0.0.1:55955
receipt: fully_satisfied True
```

→ **公開済み成果物そのものが、開発ツール無しで既製 Capsule を実行し Contract を満たす**。
ad-hoc 署名（Team ID なし）でも Formula 経由で動作するため、初期リリースで公証を必須としない
判断と整合する。残る未測定は `brew install` の実行のみ。

## 2. Kev-0.8B の実測（PASS）

### 導入
```
$ ato model install kev-0.8b --home <ATO_HOME>
kev-0.8b: 1835327775 of 1835330682 bytes downloaded into <ATO_HOME>/models/kev-0.8b
$ ato model status --verify --json
installed True bytes 1835330682 / 1835330682
all verified True
```
- 上流: `jaredpalmer/kev-0.8b@bf75a6a8`（adapter/head/tokenizer）+ `Qwen/Qwen3.5-0.8B-Base@dc7cdfe2`
- 中断→再実行で `resuming base/model.safetensors at 243038328 bytes` から Range 再開
- 独立照合: `head.pt f400bd12…920b6`、`adapter_model.safetensors 9b908623…885a`（model card の Released weights と一致）

### 推論
```
$ ato model score --home <ATO_HOME> --python <venv>/bin/python --request '<json>' --json
answers.team   = {choice: billing, confidence: 0.7764, probabilities: {billing 0.8509, returns 0.1302, shipping 0.0189}}
answers.urgent = {noul: 0.449}
runtime        = {backend: mlx, device: mps, mlx: 0.32.4, mlx_lm: 0.31.3, python: 3.12.4, load_seconds: 3.27, inference_ms: 2254.5}
checkpoint     = {weights: lora, base_revision: dc7cdfe2…, head_dim: 256, temperature: 2.3510958125672174}
```
- 3回連続で確率が完全一致（決定的）
- `temperature 2.3511` は model card の T=2.35 と一致
- **ネットワークアクセスなし**: 参照ランタイムが Hub id で解決する Qwen base を検証済みローカルディレクトリへ差し替え

### 2b. 受入条件2を release binary で再検証（2026-11-18）

デバッグビルドではなく **release プロファイルの binary**（`ato 0.9.5`）で、まっさらな ATO_HOME に
1.8 GB をダウンロードし、digest 照合と推論まで通した。

```
$ ./target/release/ato model install kev-0.8b --home <fresh>
kev-0.8b: 1835330682 of 1835330682 bytes downloaded into <fresh>/models/kev-0.8b
real 10m32.144s

$ ./target/release/ato model status --home <fresh> --verify --json
installed True | bytes 1835330682 | all verified True

$ ./target/release/ato model score --home <fresh> --python <venv>/bin/python --request '<json>' --json
choice: billing | conf: 0.6645 | backend: mlx | temp: 2.3511
```

- **release binary でも 12 asset / 1,835,330,682 bytes を完全に照合**（`all verified True`）
- 推論も動作し、`temperature 2.3511`（model card の T=2.35）を `head.pt` から読み出している
- 所要時間 10分32秒（1.8 GB、ネットワーク次第）

### ローカル決定サーバ
```
$ ato model serve --home <ATO_HOME> --python <venv>/bin/python --port 8202
$ curl -X POST .../v1/systemone -d '<decision_request>'
{"answers":{"decision":{"choice":"c3","confidence":0.4281,...}},"model":"kev-0.8b",...}
```
常駐ワーカー化によるレイテンシ実測（同一入力3回）:

| # | 実時間 | load_seconds | inference_ms |
|---|---|---|---|
| 1 | 8.64 s | 3.861 | 2167.4 |
| 2 | 0.12 s | 3.861 | 107.5 |
| 3 | 0.19 s | 3.861 | 168.5 |

`load_seconds` が同一 = 1回だけロードしている証拠。

### Formation への接続
- `kev` decision provider を `jev` から identity 分離（base URL / model接頭辞 / evidence `provider`）
- jev系応答は kev provider で invalid（relabelしない）
- 非loopback URL に鍵が無い場合は送信前に拒否
- `ato form --decision-provider kev` で選択可能。**実E2Eは Runtime Network（ato-api）が必要で未測定**

### 上流 attestation の所見
- `provenance.json` の `head_sha256` は `bc77488a…` だが公開 LFS 実体は `f400bd12…`
- `source_hashes` 30件のうち tag `kev-1.0`(=6b719c3c) の実ファイルと照合した5件がすべて不一致
- → **モデルリポジトリの provenance は実行時ソースの attestation に使えない**。Ato は公開 LFS digest と git tag で固定する

## 3. Hub参照の実測（PARTIAL）

```
$ ATO_API_URL=https://staging.api.ato.run ato run capsules/koh0920/interop-static --no-open --verification-receipt r.json
Resolved capsules/koh0920/interop-static [tag=latest] -> sha256:95b837f4…bdd813
Capsule: sha256:b565e9d7…b741db   Route: sha256:87a0ac84…b7a3cb   Runtime: static web
receipt: fully_satisfied True
```
- ローカル sample と同一の K/D。Hub から取得した束でも Contract 検証が通る
- `ato app import capsules/…` は **ローカルファイルのみ**（`error: accepts only a portable .capsule file`）→ PR #1877 待ち
- dev の `ato app start` は `worker process start time is unavailable`（サンドボックスが `/bin/ps` を拒否）→ PR #1876 の libproc で解消済み

## 3b. Hub参照 → durable Instance の実測（受入条件3の完成）— 2026-11-18

dev の `ato app install` は「ローカルファイルまたは Hub 参照」から durable Instance を作れる。
staging Hub の実在 capsule で **install → 起動 → 停止 → 再起動** を通した。

```
$ ATO_API_URL=https://staging.api.ato.run ato app install capsules/koh0920/interop-static
Route sha256:87a0ac84… requires no host permissions.
Imported Instance: linst_5ac3fbd75b8becf2ce4ca33a2f9295af4b14c59f9845676eee1f24b8f5aca1b2
Run: lrun_15d6f32018fa2c55…
Capsule: sha256:b565e9d7…b741db
Route:   sha256:87a0ac84…b7a3cb
URL: http://127.0.0.1:63841
Capsule Hub: run recorded for capsules/koh0920/interop-static
Installed. Restart with: ato app start linst_5ac3fbd7…

$ ato app inspect linst_5ac3fbd7…
active_run: lrun_15d6f32018fa2c5 | url: http://127.0.0.1:63841

$ ato app stop linst_5ac3fbd7…
Stopped Run lrun_15d6f32018fa2c55… for Instance linst_5ac3fbd7…

$ ato app start linst_5ac3fbd7…
Run: lrun_500d9bc006b38da4…
URL: http://127.0.0.1:63879
$ curl -sS -o /dev/null -w '%{http_code}' http://127.0.0.1:63879/   → 200（HTML を配信）

$ ato app stop linst_5ac3fbd7…
$ ato app inspect linst_5ac3fbd7…   → active_run: None
```

- **Hub 参照から durable Instance を作り、起動・停止・再起動できる**ことを実測（受入条件3の Hub 半分）
- 再起動は**新しい Run と新しい endpoint**（:63841 → :63879）を発行する
- 停止後は `active_run` が `None` に戻る（状態復元の確認）
- これにより PR #1877（`app add`）は dev の `app install` と重複するため close した

## 4. ソースから実行（既知D）の実測 — 公開済み v0.9.5 で再測定

GitHub の実リポジトリ（`octocat/Spoon-Knife` @ `d0dd1f61b33d64e29d8bc1372a94ef6a2fee76a9`）を取得し、
**公開済み v0.9.5 バイナリ**で Formation を実行した。

```
$ curl -L -o src.tar.gz https://codeload.github.com/octocat/Spoon-Knife/tar.gz/d0dd1f61b33d64e29d8bc1372a94ef6a2fee76a9
$ ./ato form .tmp/gh/src --runtime local --network denied --max-attempts 4 --out out --work-root work
status:        formed
candidate:     static-files/v1
contract_ref:  sha256:872f2b8eb7501c02a…
derivation_ref sha256:c1bbb7041d875f988…
receipt:       fully_satisfied: true
outcomes:      seal=succeeded, runtime_verification=succeeded, cleanup=succeeded, publication=succeeded
```

- ソースに `capsule.toml` が無い状態から、Formation が候補Dを導出し、**ビルド・実行・Contract 観測まで通して**
  `fully_satisfied: true` の receipt を得た。これが「既知Dで実行できるOSS」の受入証跡。
- 検証済み成果物は content-addressed bundle（`out/bundles/<sha>/manifest.json` + blobs）として保持される。
  `ato run` は `.capsule` ファイルのみを受け付けるため、配布用の束にするには authoring 入力
  （`capsule.toml`）を持つソースで `ato pack` する経路になる。
- **LLM生成Dが必要なOSS**の経路は未測定（下記「阻害要因」）。

## 4b. Runtime Network（hosted）経路の実測 — 2026-11-18

`ato form --runtime-network` を staging に対して実行し、requester 側の経路がどこまで通るかを確認した。

```
$ ato form samples/interop-static-k --runtime-network \
    --api https://staging.api.ato.run --token-file <token> --work-root .tmp/rn-work
satisfy 01M7NMRSN8HYEW7NRKKDHAK7GK (search search_e9deb0d18f71b219b17d96b8db091332): 0 candidate(s) admissible
error: ... /v1/runtime-network/satisfy/01M7NMRSN8HYEW7NRKKDHAK7GK: connection reset by peer
```

- **search の作成と satisfy の要求までは成功**（staging が search id と satisfy id を発行）
- **admissible な候補が 0** = このアカウントに **登録済み Runtime が無い**ため、実行できるホストが存在しない
- 続くポーリングは接続リセット（transient）

### 意味すること
- hosted 経路（受入条件4の「Runtime Network 上で検証・実行」）は、**アカウントに Runtime を登録**して初めて成立する。
  登録は `ato runtime-network serve` 側の運用で、この端末単独では完結しない。
- ローカル経路（`ato form <dir>`）は既に **PASS**（本doc §4）。初期ユーザーの単独価値はローカル経路で成立している。
- したがって hosted 経路は「未測定」ではなく「**Runtime 未登録により候補0**」という具体的な状態として記録する。

### 4c. hosted Runtime Network が止まっている理由（2026-11-18 追記）

`GET /v1/runtime-network/runtimes`（staging, account token）で実態を確認した。

```
runtime 01M46ATG90JM | user_managed | linux/aarch64 | online: false (observed 2026-10-05)
runtime rnr_01M66HBY | ato_managed  | —             | online: false (observed 2026-11-14)
```

- アカウントには Runtime が**登録されている**が、**両方とも offline**。これが「0 candidate(s) admissible」の理由。
- 登録済み Runtime を online にするには `ato runtime-network serve` に **runner token** が必要。
- このホストにある runner 資格情報は `~/.ato/runner/credentials.json` の1つだけで、`api_base = https://api.ato.run`（**本番**）。
- 本番は最終承認まで保留のため、**本番 runner token は使用しない**。staging 用の runner token は未発行。

→ hosted 経路（受入条件4の検証・実行）は「Runtime 未登録」ではなく
「**登録済み Runtime が offline で、それを online にする staging 用 runner token が無い**」が正確な阻害要因。

## 4. ソースから実行の阻害要因（要判断）

- 既知D経路は実測済み（GitHub URL → source fetch → form → pack → run → receipt）
- **LLM生成D経路は未測定**。CLI の生成プロバイダは `jev`（TypeSafe API）のみで、
  `ATO_GENERATION_JEV_API_KEY` が必要。Ato 公式API経路は CLI に未実装。
- 指示書の「デフォルトはAto公式API」を満たすには、次のいずれかの判断が必要:
  1. 公式API（ato-api）の生成エンドポイント契約を確定し、CLI に `ato` 生成プロバイダを実装する
  2. 外部API（Jev）の鍵を用意し、`--generation-provider jev` で測定する
  3. ローカルLLM経路を実装する（本ホストには mlx-lm 0.31.3 と Kev ランタイムがある）
- いずれも**権限・契約の判断**が必要で、エージェントが回避してはならない領域

## 5. ローカル管理 UI の実測（受入条件5のUI側）— 2026-11-18

初期リリースの管理UIは `ato ui`（loopback + PWA 再利用）と決定済み。PR #1876 のブランチで実測した。

```
$ ato ui --port 8301
$ curl -sS http://127.0.0.1:8301/            → 200（起動 / 停止 / 開く / 登録解除 のボタンを含む）
$ curl -sS http://127.0.0.1:8301/api/instances
  instances: 1 | state: stopped

$ curl -X POST http://127.0.0.1:8301/api/instances/<id>/start   → 200
  state: running | url: http://127.0.0.1:64064
$ curl -sS -o /dev/null -w '%{http_code}' http://127.0.0.1:64064/   → 200（アプリが配信されている）

$ curl -X POST http://127.0.0.1:8301/api/instances/<id>/stop    → 200
  state: stopped | active_run: None
```

- ページは CLI の薄いフロントエンドで、一覧・起動・停止・開く・登録解除を CLI サブコマンド経由で実行する
  （状態の権威は CLI のまま）
- **UI から起動 → 実際にアプリが配信 → UI から停止 → 状態が戻る**までを実測

### 併せて見つけた不具合と修正

`ato doctor` の `process-identity` チェックが `ps` を直接叩いており、ランタイムが libproc に移った後も
「durable Instance を起動できない」と誤って報告していた（実際は起動できる）。ランタイムと同じ
`OwnedProcessIdentity::current()` を probe するよう修正（`af5c992d3`）。

```
修正前: process-identity fail  "ps is unavailable: Operation not permitted"
修正後: process-identity pass  "pid 3275 started 1794950136.968636 (pgid 3127, boot …)"
```

## 5. 未着手 / 別repo

- Desktop UI（受入条件5の後半）: `ato-desktop` は別repo。ローカル管理UIは PR #1876 の `ato ui`（loopback + PWA再利用）で代替中
- 署名・公証: 初期リリースは Homebrew Formula 経由で**公証不要**と判断済み。Tauri `.app` の署名は保留
- 本番 Hub 登録: 最終承認まで保留（staging のみ）

## 再現コマンド

リリース成果物に対する受入チェックはスクリプト化してある（`.tmp/` 以外に書き込まない）:

```bash
cargo build --release -p ato-cli
scripts/acceptance/macos-selfhost-e2e.sh                 # binary / canned capsule / model status
scripts/acceptance/macos-selfhost-e2e.sh --hub           # + Hub 参照（ATO_API_URL / ATO_API_TOKEN が必要）
scripts/acceptance/macos-selfhost-e2e.sh --github        # + GitHub source（source fetch を持つ binary のみ）
```

実測（2026-11-18, release binary）:

```
PASS  binary runs: ato 0.9.4-stg
PASS  links only system libraries
PASS  canned capsule: fully_satisfied
PASS  model status: 12 pinned assets
PASS  hub reference: fully_satisfied
SKIP  github source (this binary has no 'source fetch')
```

## 再現コマンド（個別）

```bash
# Kev の受入を再測定する
ATO_KEV_TEST_PYTHON=<venv>/bin/python ATO_KEV_TEST_HOME=<ATO_HOME> \
  cargo test -p ato-cli --test model_inference -- --ignored --nocapture

# Hub 参照の実行
ATO_API_URL=https://staging.api.ato.run ato run capsules/koh0920/interop-static --no-open
```

## 6. 受入チェックと PR の対応（マージ後の1回で全項目を検証する）

`scripts/acceptance/macos-selfhost-e2e.sh` の各チェックが、どのPRの成果物に依存しているか。
**すべてのPRが dev に入った後の release binary で1回実行すれば、受入条件1〜5をまとめて検証できる。**

| チェック | 依存するPR | 現在の状態（dev ベースの binary） |
|---|---|---|
| 1. binary が動く / system library のみ | — | PASS |
| 2. canned capsule が `fully_satisfied` | — | PASS |
| 3. `model status` が 12 asset を返す | #1893 | PASS（#1893 のブランチで実測） |
| 4. Hub参照が `fully_satisfied` | — | PASS |
| 5. GitHub source の fetch と commit 固定 | #1880 | SKIP（未搭載） |
| 6. Hub参照 → durable Instance → 停止 → 再起動 → 配信 | #1876（libproc） | SKIP（`/bin/ps` 制限） |
| 7. 管理ページ（`ato ui`） | #1876 | SKIP（未搭載） |

### マージ後に実行するコマンド

```bash
cargo build --release -p ato-cli
ATO_API_URL=https://staging.api.ato.run ATO_API_TOKEN=<token> \
  scripts/acceptance/macos-selfhost-e2e.sh --hub --github --ui
```

期待される出力（全項目 PASS）:

```
PASS  binary runs: ato <version>
PASS  links only system libraries
PASS  canned capsule: fully_satisfied
PASS  model status: 12 pinned assets
PASS  hub reference: fully_satisfied
PASS  github source: fetched and pinned
PASS  durable Hub Instance: <linst_…>
PASS  durable Hub Instance: stopped
PASS  durable Hub Instance: restarted and serving (http://127.0.0.1:<port>)
PASS  management page: serves the Instance controls
PASS  management page: lists Instances
all checks passed
```

### 現在のオープンPR（すべて Draft）

| PR | 内容 | CI |
|---|---|---|
| #1876 | local Instance list/open/remove, `ato doctor`, `ato ui`, libproc | macOS・ubuntu 緑 |
| #1880 | GitHub URL → app（dev base, 3ファイル） | macOS・ubuntu 緑 |
| #1909 | ローカルMLXモデルを生成エンドポイント化 | macOS・ubuntu 緑 |
| #1893 | Kev-0.8B（install / score / serve / provider分離） | ubuntu緑 / macOSは既知flaky |
| #1908 | 生成プロバイダの接続先選択 | — |
| #1907 | 本ドキュメントと受入スクリプト | docs |

### 4d. 訂正: `ato app install <github-url>` は Formation 探索ではない（2026-11-18）

PR #1880 の `github_source_bundle` を読み直した結果、**GitHub URL からの install は Formation 探索を行わない**。

```rust
// 実際の処理: 静的エントリを探し、capsule.toml を書いて束にするだけ
let entry = STATIC_ENTRIES.iter().map(|name| fetched.root.join(name))
    .find(|candidate| candidate.is_file())
    .ok_or_else(|| anyhow!("unsupported-source: … has no static entry document (index.html); this release forms static sites only"))?;
fs::write(application_root.join("capsule.toml"), static_route_authoring(title, &entry))?;
let (bytes, _bundle) = build_authored_bundle_v2(&application_root)?;
```

- **探索（候補Dの列挙・試行）はしない**。`index.html` がある静的サイトだけを、固定の static route として束ねる。
- したがって「GitHub URL → 実行」の**近道**ではあるが、受入条件4の「Formationを探索・生成」の証跡にはならない。
- 受入条件4の「既知Dで実行できるOSS」の証跡は、**`ato form <dir>`**（候補Dを試行し receipt を出す）の実測（§4）である。
- 「LLM生成D」は Runtime Network 上の生成経路のみで、staging runner token が必要（§4b/§4c）。

**この訂正により、受入条件4の判定は次のとおり:**
- 既知D: **PASS**（`ato form` の実測）
- GitHub URL からの単一コマンド: **静的サイト限定の近道**（探索なし）
- LLM生成D: **未測定**（runner token 待ち）

## 7. Kev の順位付けは hosted 経路にしか繋がっていない（仕様との差分）

指示書の役割分担は「Kev-0.8B：実行条件を満たす候補の評価・順位付けに使用する」。
実装を確認した結果、**Kev の decision provider は Runtime Network 経路にのみ接続されている**。

```rust
// apps/cli/src/lib.rs
#[arg(long, value_parser = ["jev", "kev"], requires = "runtime_network")]
decision_provider: Option<String>,
```

- `ato form --decision-provider kev` は `--runtime-network` を要求するため、**ローカルの `ato form` では候補の順位付けに Kev を使えない**
- ローカル経路は `--max-attempts` の範囲で候補を順に試すだけで、順位付けの判断点が無い
- したがって「Kev で候補Dを評価・順位付けする」は、hosted Runtime Network が online な Runtime を持つ場合にのみ成立する

### 現状の評価

| 要素 | 状態 |
|---|---|
| Kev の導入・推論・ローカルサーバ | **実装済み・実測済み**（§2） |
| `kev` decision provider（`jev` と identity 分離） | **実装済み・テスト済み**（PR #1893） |
| Kev による候補Dの順位付け（ローカル経路） | **未実装**（decision provider が `--runtime-network` 必須） |
| Kev による候補Dの順位付け（hosted 経路） | 実装済みだが Runtime が offline のため未測定 |

### 初期リリースへの影響

受入条件2（任意DL + ローカル推論）は満たしているが、**roadmap 6 の「候補Dの評価・順位付け」は
ローカルでは未接続**。初期リリースで Kev を「順位付けに使う」と謳うには、ローカル `ato form` に
decision point を設ける追加工事が必要（別PR）。

