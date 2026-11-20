# 引き継ぎ: Capsule Hub P0アプリ実機検証（2026-11-20 23:00 JST 時点）

新しいチャットでこの作業を続けるための引き継ぎ。まず本書→ `docs/ops/capsule-hub-p0-20261119.md` → 証跡CSVの順に読む。

## 1. 目的と完了条件

staging Hub（`https://stg-app.ato.run/capsules` / `https://staging.api.ato.run`）に登録された **58アプリすべてを「実際に使えるソフトウェア」として確認する**。起動やContract成功だけでは完了にしない。1アプリにつき次の9段階を通す。

1. Hub参照の解決 → 2. 宣言Derivation（D）の選択 → 3. 依存・Bindingの充足 → 4. 起動 → 5. Contract観測 → 6. Surface（画面/エンドポイント）到達 → 7. **主要操作**（アプリ固有の中心機能）→ 8. 保存と再読み込み（宣言スコープ内）→ 9. 停止と後片付け

各アプリで記録する: Hub URL/ref、Bundle SHA-256、ContractRef、DerivationRef、CLIビルドSHA、OS/arch、Runner、Binding、実行方式、時刻、最初に失敗した段階とstderr/終了コード、原因分類（CLI / Derivation / ホスト / 外部依存 / 上流 / セキュリティ / 調査不足）、次のアクション。

## 2. 現在の検証状況（増分0のまま）

| 区分 | 件数 |
|---|---:|
| 主要操作まで確認済み | 17 |
| Contract観測のみ | 2（Stirling-PDF, AppFlowy） |
| 起動未確認 | 39 |

- 台帳（正）: `docs/ops/evidence/capsule-hub-p0-20261119/p0-status-20261120.tsv`（アプリ別の現状・Binding・失敗段階・分類・次のアクション）
- 元台帳: 同ディレクトリ `catalog.csv` / `runtime.csv` / `runtime-receipts.json`
- staging再照合: `stg-redigest-20261120.tsv`（台帳digestと現行latestの一致/不一致）、`bundle-facts-20261120.tsv`、`search-api-probe-20261120.md`
- 実機Run記録: `run-20261120-night.md`
- **注意**: 突き合わせは必ずHub参照とdigestで行う（アプリ名は表記ゆれで一致しない）。digestが変わった10件（Gitea, Uptime Kuma, SiYuan, Homepage, Super Productivity, Redmine, WordPress, PostgreSQL, LibreOffice, Mermaid）は「台帳の初期catalogと違う」だけで、後のreceiptで確認済みの可能性がある。変更と断定する前に`runtime-receipts.json`と照合する。

## 3. 実行環境

- macOS（Darwin arm64）。Docker Desktop 4.99.0 を再インストール済み、Engine 29.9.3、VMは`linux/aarch64`で`linux/amd64`イメージをエミュレーション。**エミュレーションは遅いが動く**（receiptに記録される）。
- ディスク: 空き約206GB。1件ごとにコンテナ・イメージ・Runデータを回収する運用（下記）。
- CLIは2種類あり、**必ず区別して記録する**。

| 用途 | パス | SHA-256 | 特徴 |
|---|---|---|---|
| 配布版（stagingの正式） | `.tmp/p0-096/bin/ato-cli-aarch64-apple-darwin/ato` | `c41e4a627cab…` | 0.9.6-stg。**単一コンテナOCIをrelay未対応として拒否**する |
| 検証版（PR #1921） | `.worktrees/hub-p0-cli/target/release/ato` | `7ffe014bb8f6…` | 0.9.6-stg + #1912の修正5件。単一OCI relayが動く |

- 状態ディレクトリは`ATO_HOME`でワークスペース内に固定する（実ユーザーの`~/.ato`は触らない）。
- バンドルキャッシュ: `.tmp/p0-096/ato-home/hub/*.capsule`（58件取得済み・digest検証済み）。

## 4. 未デプロイの変更（すべてDraft・base=dev・マージもデプロイもしていない）

| PR | 内容 | 状態 |
|---|---|---|
| [ato #1920](https://github.com/ato-run/ato/pull/1920) | 台帳・証跡・引き継ぎ（本ファイル） | 更新継続中 |
| [ato #1921](https://github.com/ato-run/ato/pull/1921) | #1912の修正をdevへ意味統合（単一OCI relay、署名framework復元、IPC、scratch volume、起動観測admission、4GiB署名DMG） | 検証用バイナリの供給元 |
| [ato #1922](https://github.com/ato-run/ato/pull/1922) | CLI一般改善7項目（未割当マウント診断、Port無しworker、Bundle上限表示、pull失敗詳細、Binding案内、環境ラベル検証、hub --help区分） | 実装・テスト完了、実pullは未検証 |
| [ato-api #1324](https://github.com/ato-run/ato-api/pull/1324) | サーバー側検索（q/category、フィルタ後total）とdigest単位の検証状態、verification API、migration 0428 | **配備保留（下記）** |
| [ato-pwa #1153](https://github.com/ato-run/ato-pwa/pull/1153) | 全件検索、digest単位バッジ、実行案内、デザイン調整 | **配備保留（下記）** |

### 配備を止めている理由（重要）

#1324 は「未認証の自己申告 + レート制限なし」で`primary_operations_verified`を立てられる設計で、主要操作の証明として不十分。digest・K・Dの一致だけでは発行者を認証できない。修正方針: 信頼できる観測（署名済みreceipt等）だけを根拠にし、起動観測のみのreceiptは`contract_verified`止まり、未認証の失敗・昇格投稿は不可、根拠不明は未検証のまま。#1153 は #1324 の契約に依存するため同時に止める。検索部分のみ独立して出せる。

## 5. 判明している技術的知見

- **Keycloak は起動失敗（原因特定済み）**: `start-dev` は起動時に `/opt/keycloak/lib/quarkus/transformed-bytecode.jar` を書き出すが、Ato OCIランタイムはrootfs読み取り専用（`--read-only`）なので `Read-only file system` で exit 1。`/tmp`不足でもOOMでもない。→ ビルド済みイメージを固定した新しいDで検証するのが次の一手（rootfs書込可の緩和はしない）。
- **Memos は relay 経路で起動成功**（検証版CLI）。`http://127.0.0.1:63633`、`GET /`=200、`/healthz`=Service ready.、Hubへrun記録。停止後はコンテナ・ボリューム残存なし。ただし主要操作（メモ作成）は v0.30 のサインインが `invalid credentials` で再実行できず、前回の記録を維持。
- **NocoDB は起動途中で中断**（引き継ぎのため停止）。配布版では `oci_container_relay_unsupported` と `platform_emulated` が理由に出る。検証版では起動処理まで進んだが、Contract/主要操作は未確認。
- **CLIの改善点（証跡に記録済み）**: (1) Run失敗時にコンテナのstderrが残らない→切り分け不能、(2) 停止後もイメージが残る、(3) 停止のたびに`ato app recover <id> <save|discard>`の判断が必要（`--on-exit`を使う）。

## 6. 再開手順（1件ずつ・後片付け込み）

```sh
cd /Users/egamikohsuke/Ekoh/projects/ato-run
BIN=$PWD/.worktrees/hub-p0-cli/target/release/ato     # 検証版。配布版との差は証跡に書く
REL=$PWD/.tmp/p0-096/bin/ato-cli-aarch64-apple-darwin/ato  # 配布版（起動可否の比較用）
BUNDLE=$PWD/.tmp/p0-096/ato-home/hub/<bundle-sha256>.capsule

# 1) 宣言Dと起動可否を確認（実行しない）
$REL run --list-derivations "$BUNDLE"      # 配布版の判定
$BIN run --list-derivations "$BUNDLE"      # 検証版の判定

# 2) 認証情報を含まない一時プロファイルで1件だけ起動
S=$(mktemp -d .tmp/p0-096/run.XXXXXX)
ATO_HOME=$S/home $BIN run --headless --fresh --on-exit save \
  --accept-permissions --verification-receipt "$S/receipt.json" \
  --derivation <DerivationRef> "$BUNDLE"

# 3) 主要操作と保存確認（例: 画面は http://127.0.0.1:<port>）
#    HTTP APIがあるアプリはAPIで、UIのみのアプリはブラウザで操作し、操作の前後を記録する

# 4) Ctrl-C で停止 → データ方針を決める
ATO_HOME=$S/home $BIN app recover <data-id> discard   # 検証では基本 discard

# 5) 後片付け（必ず）
docker ps -aq | xargs -r docker rm -f
docker image prune -af
docker volume prune -f
rm -rf "$S"
```

- Bindingが要るアプリは`--bind-env <id>=<ENV_VAR>`で渡す（秘密値をコマンドラインやログに残さない）。合成の検証用値を使い、実在の認証情報・個人情報は使わない。
- 1件終わるごとに上の2〜5を実行し、`p0-status-20261120.tsv`の行を実測で更新する（推測で「確認済み」にしない）。
- 主要操作が「APIで作成→APIで読み出し」で示せるなら、そのリクエストとレスポンスを証跡に残す。

## 7. 禁止・注意

- staging/本番へのデプロイ、mainへのマージ、production操作は禁止（依頼があった場合のみ）。共有stagingへfeatureブランチを直接出さない。
- 実在の資格情報・個人データを検証用ワークロードに使わない。`~/.ato`（実ユーザーの状態）を汚さない。
- Docker Desktopの停止・削除・再インストールはユーザー判断。共有イメージ削除など他作業への影響を与えない。
- 破壊的操作（`rm -rf`）は直前に対象パスを絶対パスで確認する。他タスクのworktree・記録・データを消さない。
- 「未検証」を「成功」と書かない。fixture/mockの成功を実機Acceptanceとして報告しない。

## 8. 次の優先順位

1. 検証版CLIで**未確認39件を1件ずつ**（NocoDB再開 → MediaWiki / Moodle / Matomo / Snipe-IT など認証が軽いものから）。主要操作まで通して台帳を更新。
2. Keycloak をビルド済みイメージ固定の新Dで検証し、Contract＋realm作成＋再起動後の永続性を確認。
3. digest変更10件の回帰（Super Productivity・Mermaid はprocess経路で、親サンドボックス内では起動不可 → 通常ターミナルで実行）。
4. #1324 の信頼性修正（信頼できるreceiptのみを根拠化）→ 検索のみ先行して出す判断。
5. Docker不要D（Python/process/static）の追加検証。
