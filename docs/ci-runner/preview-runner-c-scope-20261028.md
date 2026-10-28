# C 案（CI idle 時のみの Preview Runner）— 排他制御・変更範囲・承認依頼（2026-10-28）

状態: 設計・承認依頼（Draft）。**ホスト変更・VM 起動・コード適用は未実施。**
位置づけ: これは **#946 の一時的な実機検証手段**であり、PR が存在する間いつでも使える常設 Dev Preview ではない。常設化は A/B の判断後に行う。

## 1. 決定

| 案 | 判断 |
|---|---|
| C. CI idle 時のみ（本 doc） | **採用（#946 限定）**。ただし下記の排他制御が実装・実測できることが条件 |
| A. CI メモリ縮小 | **保留**。C の実績後、canary で段階的に実測してから判断 |
| B. ubuntu-sugamo へ配置 | **代替候補**。staging への影響評価が前提 |

**C 案の必須条件**: 「CI が idle であることを確認する」だけでは不十分。Preview 開始直後の GitHub 新規 Job 投入との競合を防ぐため、**ホスト側で CI VM と Preview VM の起動を排他的に制御**する。実装できない場合は C も **BLOCKED** とする。

## 2. 排他制御プロトコル（flock ベース）

既存 provisioner は単一インスタンス用の `flock(/var/lib/ato-runner/lock/provisioner.lock)` を持つ。これを拡張し、**VM ライフサイクル単位の admission gate** を追加する。

| 要素 | 内容 |
|---|---|
| Gate lock | `/var/lib/ato-runner/lock/preview-gate.lock` に対する `flock(LOCK_EX)`。**CI job 実行中は provisioner が保持**し、teardown で close して解放。**Preview 実行中は preview supervisor が保持**し、終了時に close。カーネルが close/プロセス死亡で自動解放するため、SIGKILL でも解除漏れしない |
| Gate request | `/var/lib/ato-runner/run/preview-gate.request`（JSON: preview id, pid, boot_id, expires_at）。preview supervisor が起動前に作成。provisioner は有効な request がある間、**新しい job を claim しない**（JIT 発行も行わない） |
| Provisioner の変更点 | (1) `loop()` 冒頭で gate request/lock を確認し、要求中または保持中は `find_queued_job()` を呼ばず heartbeat=`preview_gate` で待機（GitHub 側は queued のまま、runner は作られない）。(2) `find_queued_job()` 後・`run_job()` 前に `flock(LOCK_EX|LOCK_NB)` を取得。取れなければ job を捨てて待機（JIT 未発行なので副作用なし）。(3) teardown の finally で fd を close。(4) `health()` に gate 状態と preview unit/qemu を追加（preview 実行中は CI 不稼働が正常）|
| Preview supervisor の手順 | (1) request 作成 → (2) 既存 CI job の終了を待つ（`current.json` 不在 + gate lock 取得を最大 180s リトライ）→ (3) gate lock を保持したまま preview VM を起動 → (4) 実行/停止/再開/TTL を管理 → (5) teardown で VM/overlay を後始末し、request 削除 → (6) fd close で CI 再開 |
| 競合窓の排除 | provisioner は gate を取る前に request を必ず確認する。request 作成と provisioner の gate 取得が競合した場合は、provisioner が request を優先して待機する（request の expires_at で最大待機を制限）。preview が 180s 以内に gate を取れない場合は request を撤回して失敗を返し、CI を再開する |
| 異常終了 | supervisor が SIGKILL されても flock はカーネルが解放。request は pid/boot_id/expiry で無効化。preview qemu/overlay の残骸は既存 `ato-runner-recover` パターンの `ato-preview-recover`（timer または ExecStopPost）で回収。CI は最大 `expires_at` 経過後に自動再開 |
| 観測性 | 両者の heartbeat/state に gate 状態、待機理由、待機時間、preview id、runner 登録 id、build id を記録。`ato-runner-health` と preview health の両方で確認可能にする |

このプロトコルは **新しい VM 管理エンジンを作らず**、既存 provisioner の loop/run_job/teardown/health に約 60–100 行の追加で実装できる見込み。

## 3. 変更範囲（C 案・適用は承認後）

### 3.1 ホスト（sugamo-test）

| # | 変更 | 内容 | 影響 |
|---|---|---|---|
| H1 | provisioner パッチ | §2 の gate 追加。パス定数の env 化は C では不要（単一インスタンスのまま） | CI の起動待機が増える可能性。JIT/label は不変 |
| H2 | preview supervisor | `/usr/local/sbin/ato-preview-supervisor` + `ato-preview@.service`（template, `Restart=no`, `TimeoutStopSec`, `ExecStopPost`）。root 実行・`/opt/ato-preview` に配置 | 新規 unit。既存 CI unit は不変 |
| H3 | preview state | `/var/lib/ato-preview/{state,overlays,logs}`、`/run/ato-preview`、ログ retention | ディスク ~1–2G/実行（overlay 20G sparse）。TTL で回収 |
| H4 | ネットワーク | `br-pv` 10.214.0.1/24 + dnsmasq 設定 + 専用 nft table（preview API origin/OCI registry のみ許可、host/tailnet/RFC1918/metadata 拒否）+ `br_netfilter` 有効化（preview VM 間/CI 間 L2 分離） | 既存 br-ci/nft は不変。追加のみ |
| H5 | golden role | golden image の `ato-job-agent` に `ConditionPathExists=/dev/disk/by-id/virtio-ato-jit` を追加し、preview 用 role payload（config disk）で `ato-preview-agent` を起動。pinned ato runner binary + build provenance を payload に含める | golden 再ビルド 1 回。CI の挙動は不変（JIT disk がある場合のみ従来 agent） |
| H6 | ingress | preview VM 内 cloudflared quick tunnel（または承認済み最小 ingress）で surface を公開。URL は起動ごとに変わるため resume 時に再登録 | #946 の app HTTP 200 用。TTL で停止 |

### 3.2 Preview API（Cloudflare・preview のみ）

- preview D1 に**必要な migration のみ**適用（create 経路に migration 適用ステップを追加）。staging の migration を一括適用しない。
- Secret は**個別に列挙・最小限のみ**設定（staging の 37 個をコピーしない）。候補: auth/session 用 secret、`RUN_CONTROL_SESSION_ROOT_SECRET`、`CAPSULE_VALIDATOR_AGENT_TOKEN`、surface assertion keys。各 secret は「何に必要か・preview 専用に新規生成できるか・検証方法」を表にして承認を得る。
- Runner 登録は `attach-enrollments`（session auth）で preview 専用 runner token を発行し、単一使用 token を VM の config disk でのみ渡す。**GitHub Actions 登録・JIT・CI token は一切持たせない。**
- Validator は preview VM 内で preview API のみに接続する companion として起動（with-data export の ready 化）。

### 3.3 リポジトリ（Draft PR）

- provisioner gate パッチ（未適用の patch として提出）
- preview supervisor / guest preview agent / systemd unit / nft・dnsmasq 設定（未適用）
- `dev-preview.mjs` の C 用 operator コマンド（`runner-preview create/stop/resume/cleanup`、dry-run 既定）— 常設 Dev Preview の provider にはしない
- #946 E2E スクリプト（既存 `.tmp/dev-preview-runner-20261028/e2e/` を C 用に限定）

## 4. #946 実機 E2E（C 案）

1. 排他制御: preview 開始→CI 新規 VM 不発行→既存 job 完了待ち→preview 実行中は CI queued→preview 終了後に CI 再開。
2. preview 作成: 専用 VM + 14 Cloudflare 資源作成。
3. 実行: Hosted app（process/OCI）を preview Runner で実行し、app HTTP 200・receipt `fully_satisfied=true`・K/D 記録。
4. 停止/再開: overlay 保持で状態復元、runner 再登録、app 200。
5. #946: with-data export → K′ → import → receipt → stop/restart 永続。A1–A6（writer fence、timeout、idempotent retry、no partial app）を実測。
6. cleanup: VM/overlay/bridge/process/registration/Cloudflare 資源の zero residue。
7. 異常終了: supervisor/qemu SIGKILL → gate 自動解放、CI 再開、残骸回収。

KVM が無い環境・dry-run・mock は PASS にしない。deployed identity（stack/golden/preview API/PWA asset）を前後で記録する。

## 5. A 案 canary（C の後・保留中）

1. CI VM RAM を 5,632→5,120→4,608→4,300 MiB と段階的に縮小（`MemoryMax` も同期）。
2. 各段階で canary job（短時間の CI）と実ジョブを実行し、runtime・queue latency・再試行・OOM/swap・テスト失敗率を記録。
3. 併せて preview VM 2,048 MiB を同居させ、両方 green かつ host MemAvailable に安全余裕（目安 500 MiB 以上）がある場合のみ採用。
4. 少しでも悪化（OOM、swap thrash、有意な遅延増、ジョブ失敗）があれば即ロールバックし、A 不採用とする。

## 6. B 案検討時のディスク帰属確認（実測）

ubuntu-sugamo の 98% と 652GB 空きは**同一ファイルシステムの時系列変化**であり、別マウントの混同ではない。

| マウント | デバイス | サイズ | 現在 | 過去の報告 |
|---|---|---|---|---|
| `/`（ubuntu-sugamo） | ubuntu-vg/ubuntu-lv | 914G | 224G used / 652G free / **26%** | 857G used / 19G free / **98%**（snapshot 回収前） |
| `/data`, `/var/lib/ato-runner-volumes`（ubuntu-sugamo） | sda1 | 916G | 320G used / 550G free / 37% | — |
| `/`（sugamo-test） | ubuntu-vg/ubuntu-lv | 232G | 33G used / 189G free / 15% | 98% ではない |

したがって B 案の容量評価は `/` の 652G 空きと `/data` の 550G 空きを別々に扱い、staging runner の workload と同居する前提で再評価する。

## 7. 承認チェックリスト（ホスト変更の前に必要）

- [ ] §2 の排他制御プロトコル（gate lock/request、timeout、異常終了時の自動解放）の承認
- [ ] H1–H6 の適用範囲と maintenance window（CI idle 時間帯、1 回の変更で 1 項目ずつ）の承認
- [ ] golden 再ビルドの承認（CI への影響を canary で確認）
- [ ] preview API の migration と secret の**個別**リスト・生成方法の承認（staging からの一括コピー禁止）
- [ ] ingress（cloudflared quick tunnel 等）の一時利用と TTL の承認
- [ ] rollback 手順（provisioner パッチ戻し、unit 停止/削除、br-pv/nft 削除、golden 復元、preview 資源削除）の承認

いずれも承認前に適用しない。まず H1 のパッチを**停止状態でレビュー**し、次に maintenance window で H2/H4 を追加、最後に golden（H5）を更新する段階適用を推奨。

## 8. 未実施

ホスト変更、provisioner パッチ適用、VM 起動、preview API migration/secret 適用、ingress 作成、#946 E2E、A canary は行っていない。