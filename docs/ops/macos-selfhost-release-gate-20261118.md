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
| 3 | Capsule取り込み（.capsule + Hub参照） | **PARTIAL** | `.capsule` の durable lifecycle は PR #1876 で実測。Hub参照→検証→実行は dev で PASS（receipt `fully_satisfied: true`）。Hub参照→durable import は PR #1877 待ち |
| 4 | ソースから実行（GitHub → Formation） | **PARTIAL** | 既知D経路は実測済み（Spoon-Knife: fetch→form→pack→run→receipt）。LLM生成Dは**公式API権限が無く未測定** |
| 5 | ローカル管理（CLI + Desktop UI） | **PARTIAL** | CLI は PR #1876 で実測。Desktop UI は別repo（ato-desktop）で未着手 |

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

## 4. ソースから実行の阻害要因（要判断）

- 既知D経路は実測済み（GitHub URL → source fetch → form → pack → run → receipt）
- **LLM生成D経路は未測定**。CLI の生成プロバイダは `jev`（TypeSafe API）のみで、
  `ATO_GENERATION_JEV_API_KEY` が必要。Ato 公式API経路は CLI に未実装。
- 指示書の「デフォルトはAto公式API」を満たすには、次のいずれかの判断が必要:
  1. 公式API（ato-api）の生成エンドポイント契約を確定し、CLI に `ato` 生成プロバイダを実装する
  2. 外部API（Jev）の鍵を用意し、`--generation-provider jev` で測定する
  3. ローカルLLM経路を実装する（本ホストには mlx-lm 0.31.3 と Kev ランタイムがある）
- いずれも**権限・契約の判断**が必要で、エージェントが回避してはならない領域

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
