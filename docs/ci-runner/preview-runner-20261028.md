# Dev Preview Runner on the existing self-hosted CI base — audit, design and gate (2026-10-28)

状態: 監査・設計（Draft PR）。**実装・VM 起動・ホスト変更・deploy は行っていない。** 対象は既存 CI 基盤（sugamo-test）と ato-api Dev Preview CLI。
結論: **Phase 1 監査は PASS（基盤再利用可）／PARTIAL（同時利用）／BLOCKED（Preview スタック未実装）。CI Job と Preview VM の同時実行は現行ホスト構成では容量不足で BLOCKED。** 実負荷試験の前に停止し、最小変更と代替案を提示する。

## 0. 制約と方法

- Production 変更、staging のユーザーデータ削除、既存 CI 移行の上書き、Portable Capsule v3/v4 仕様変更、force push、新規物理サーバー・有料サービス契約は行わない。
- 監査はすべて read-only。`ssh user@100.123.180.75` + `sudo -n` の読み取り、ローカルに写した stack（`.tmp/dev-preview-runner-20261028/host/`、実機と 8/8 sha256 一致）の精査、コード参照、既存 E2E 証跡の再利用のみ。VM 起動・設定変更・service 再起動・削除はしていない。
- 詳細証跡: `.tmp/dev-preview-runner-20261028/{audit,e2e,release}/`。

## 1. 環境（実測 2026-10-28T09:04–09:10Z）

| 項目 | 値 |
|---|---|
| CI ホスト | sugamo-test（tailscale `100.123.180.75`、LAN 192.168.0.92）。**public IPv4 なし** |
| CPU / RAM | 6 vCPU（i5-8400 6C/6T、SMT なし）/ 7,786 MiB + 8 GiB swap |
| ディスク | `/` 232G、33G used、189G free（**15%**） |
| Hypervisor | `/dev/kvm`、QEMU 8.2.2、cgroup v2、systemd 255 |
| CI stack | `/opt/ato-runner` VERSION 1.0.0、provisioner/net/dnsmasq/health.timer active、golden.qcow2 sha256 `e4cea6bf…` 6447456384B mode 0444 |
| CI VM | 6 vCPU / 5632 MiB / 40G sparse overlay + 8 GiB in-guest swap、`MemoryMax=6656M`、`CPUQuota=500%`、concurrency 1（flock） |
| CI ネットワーク | `br-ci` 10.213.0.1/24 + dnsmasq、nft `inet ato_ci_runner` で RFC1918・100.64/10・169.254/16・metadata を遮断、egress は tcp 22/80/443/9418 + udp 123/443 のみ、host→br-ci drop |
| Dev Preview | ato-api `scripts/dev-preview.mjs`（origin/stg `d95fab7f`）、preview あたり Cloudflare 14 資源 |

**98% ディスク警告の帰属**: 該当は **ubuntu-sugamo（staging runner host, 100.114.96.42）** であり、sugamo-test ではない。ubuntu-sugamo は回収後 26%（652G free）、sugamo-test は 15% で逼迫なし。

## 2. Phase 1 監査結果（PASS / PARTIAL / BLOCKED）

| 領域 | 判定 | 要点 |
|---|---|---|
| CI 基盤の再利用可能性 | **PASS** | QEMU/overlay/QMP/scope/serial marker/timeout/teardown/recover/golden 検証/nft・dnsmasq テンプレートを再利用可。VM 起動→job 開始 16s（4/4）、JIT は 10s で shred、overlay は job 後回収、残留 0 |
| CI Job と Preview VM の同時実行 | **PARTIAL → BLOCKED**（容量） | CI の cgroup ceiling 6,656 MiB が host 7,786 MiB の大半を占め、preview VM を安全に同居させられない（§3） |
| Preview スタック | **BLOCKED** | preview supervisor/guest agent/unit/bridge/state/golden role が存在しない |
| Dev Preview（Cloudflare 資源） | **PASS** | 14 資源/preview、冪等 create/resume/stop/cleanup/TTL、name-suffix guard、staging id 混入 0。P0-4 2 ブランチ実 E2E PASS |
| Preview API の前提 | **BLOCKED** | create 経路は D1 migration を適用せず（preview D1 空）、約 37 secrets 未移植、`env.*` は drop。runner 登録・run-control・browser run-control が成立しない |
| Runner 接続プロトコル | **PASS（コード）** | `POST /v1/runners/attach-enrollments` → `POST /v1/runners/enroll` → heartbeat 30s/online 90s → lease claim TTL 15m。CRW 側の flag/heartbeat と一致 |
| Preview VM の runtime 契約 | **PARTIAL** | CRW 再利用は妥当。pinned ato バイナリ・docker/bwrap+Landlock・build provenance が未整備 |
| Public surface | **BLOCKED** | runner の `public_base_url` 配下に `ready_url` が必要で CF app proxy が dial するが、host は tailnet のみで cloudflared/tunnel なし → Cloudflare から preview runner surface に到達不能（app 200 が取れない） |
| Validator companion | **BLOCKED** | with-data export の ready 化は別 Rust validator が export job を claim する必要があり、preview に無い（`CAPSULE_VALIDATOR_AGENT_TOKEN` 前提） |
| 分離（CI VM → host） | **PASS（network/service）+ PARTIAL（host hardening）** | nft で br-ci から host/tailnet/RFC1918/metadata を遮断、host→br-ci drop、provisioner に inbound listener 無し。一方 provisioner は root かつ `NoNewPrivileges=no` 等の hardening なし |
| 分離（Preview VM → CI creds） | **BLOCKED** | Preview VM が未実装で検証不能。設計では GitHub 登録/JIT/CI token を一切持たせない |
| 分離（Preview → staging/prod） | **PARTIAL** | Cloudflare 資源分離は P0-4 で PASS。runtime の egress は未実装で、preview API のみを allow する専用 nft が必要 |
| 分離（2 preview 間） | **PARTIAL** | 資源/session 分離は PASS。同一 bridge 上の VM 間 L2 は `br_netfilter` 未ロードのため filter されず、専用 br-pv と bridge 分離が必要 |
| E2E harness | **設計 PASS / 実行 BLOCKED** | 12 シナリオ・verdict schema・dry-run スクリプトを用意。KVM を hard gate とし、mock/dry-run を PASS にしない |
| Release 統合 | **設計 PASS / ゲート BLOCKED** | Manifest 追加フィールド案（PROPOSAL / NOT APPLIED）と rollback L0–L4 を用意。#946 G1 は未達 |

## 3. 容量（同時実行の可否）

実測: host 7,786 MiB、idle MemAvailable 7,222 MiB、CI cgroup ceiling 6,656 MiB（VM RAM 5,632 MiB + swap 2,048 MiB）。

| 構成 | 合計（MiB） | 判定 |
|---|---|---|
| CI + preview S（1,152） | 7,808 | 既に host 超過（不安全） |
| CI + preview M（2,176） | 8,832 | 1,044 over → swap thrash / OOM 危険 |
| CI + preview L（3,072） | 9,984 | 不可能 |
| preview のみ 2×M | 4,352 | 安全（推奨 caps: 2 vCPU / 2,048 MiB / MemoryMax 2,688M / 32G overlay / TTL ≤2h） |
| preview のみ 3×S | 3,456 | 安全（3×M は際どい） |

CPU も CI 500% + preview 2 vCPU = 7/6 で oversubscribe。CI の queue latency は実測 30–498s で、悪化は CI 全体に影響する。

## 4. 最小アーキテクチャ（Phase 2・実装は未着手）

**共通**: 既存 sugamo-test、KVM、QEMU、golden/overlay、systemd scope、nft/dnsmasq、recover/health のパターンを再利用。CI 用 GitHub Actions JIT Runner と Preview 用 Runner は**別 VM**。

- **CI 専用**: 現行 provisioner（GitHub JIT）をそのまま維持。CI VM の内部から nested KVM は使わない。
- **Preview 専用**: 同一 supervisor コードを role=preview で起動する別 instance（または同 provisioner の preview mode）。GitHub 登録・JIT・CI token を持たない。guest は role payload で preview-runner として起動し、**preview API のみ**に attach-enrollment（単一使用 token を read-only config disk で搬送）で登録する。
- **管理**: ホスト側 Controller は root-owned のローカルコマンド（operator が Tailscale SSH 経由で実行）。CI Job からは到達不能。新しい inbound API は作らない。
- **Preview 資源**: 既存 Dev Preview の 14 Cloudflare 資源（D1/R2/KV/API/PWA/instance/volume 記録）を再利用し、VM と runner 登録は preview id で名前空間化する。
- **VM**: 1–2 vCPU / 2,048 MiB（cgroup 2,688M）/ 32G sparse overlay（stop で保持、cleanup で削除）/ golden は read-only。
- **ネットワーク**: 専用 `br-pv`（例 10.214.0.1/24）+ dnsmasq + 専用 nft（preview API origin・OCI registry・必要最小限のみ許可、host/tailnet/RFC1918/metadata は拒否）。CI の br-ci とは分離し、2 preview 間の L2 も分離する（br_netfilter 有効化または per-VM netns）。
- **ライフサイクル**: create → VM up → enrollment → runner ready → app run → stop（overlay 保持）→ resume → TTL/PR close cleanup。停止・削除・再試行は preview id に対して冪等。CI の job 進行を阻害しない（共有 admission lock と time-slice）。

**同じ物理ホストで同時実行する場合の必須条件**: 容量ポリシー（§5 のいずれか） + 専用 bridge/nft + preview API の secret/migration + public surface + validator。これらが揃うまで実装・負荷試験は行わない。

## 5. 容量に関する最小代替案（要判断）

| 案 | 内容 | 長所 | 短所 / 必要な承認 |
|---|---|---|---|
| **A. CI プロファイル縮小** | CI VM RAM を 5,632→約 4,300 MiB、`MemoryMax` を引き下げ、preview 2,048 MiB を同居。admission で preview 1 件。 | 同一ホストで「CI+Preview 同時」の最終条件を満たせる可能性。追加費用 0。 | CI の Rust テスト性能/安定性に影響。canary job での実測と CI owner 承認が必要。失敗時は即 rollback。 |
| **B. Preview を ubuntu-sugamo へ** | 既存 ubuntu-sugamo（12 vCPU / 30 GiB / 652G free / KVM）に同じ QEMU/overlay パターンで preview VM を置く。br-pv/nft で CI と完全分離。 | 容量余裕。既存物理ホストで追加費用 0。CI と物理分離。 | staging runner と同居するため blast radius の再評価が必要（VM から host/credentials へ到達不可の検証、専用 bridge、host 硬化）。staging 運用への影響承認が必要。 |
| **C. time-sliced（CI idle 時のみ）** | sugamo-test で CI idle + queue 空 + qemu 無しのときだけ preview VM を起動（lease 共有）。 | 追加ハード不要。preview の作成・実行・再開・削除と #946 Runner 検証は可能。 | 「CI と同時実行」の最終条件は満たさない。preview は CI 中停止待ち。 |

推奨: **#946 の Runner 検証は C で先行**し、最終条件（同時実行）は **A の canary 実測**で判断。A が不合格なら B を設計（staging 影響評価込み）。いずれもホスト変更・設定変更は承認後に実施する。

## 6. Phase 3 で必要な最小変更（PROPOSAL / 未適用）

1. provisioner のパス定数（L49–65）を env でパラメータ化し、preview instance を同一コードで起動可能にする（新 engine は作らない）。
2. preview mode を追加: GitHub poll の代わりに root-owned request file（operator が SSH で作成）を読み、QMP down で overlay を保持、TTL/異常終了 recovery を実装。
3. guest role payload（config disk）で `ato-job-agent` を無効化し preview-runner agent を起動。golden へ pinned ato バイナリ + provenance を追加（または role payload で搬送）。
4. `br-pv` + dnsmasq + 専用 nft + bridge 分離（br_netfilter 有効化 or netns）。health に bridge/nft/iptables 検査を追加。
5. Dev Preview create 経路: preview API の D1 migration 適用と preview-scoped secrets（runner/validator/surface/run-control）を operator 承認付きで追加。
6. `dev-preview.mjs` に `--provider runner-host`（既存 provider インタフェース）を追加し、VM 作成/停止/再開/cleanup を preview 資源と同一 id で束ねる。enrollment token は単一使用で VM にのみ渡す。
7. public surface: cloudflared quick tunnel か、承認済みの公開 ingress を preview runner 用に用意（Cloudflare から到達可能にする）。
8. validator companion を preview に配置（with-data export の ready 化）。

## 7. Phase 4 E2E（設計済み・実行は前提条件待ち）

- 12 シナリオ: P0 preflight / S1 preview 資源 / S2 VM 起動 / S3 enroll+build pin / S4 process run / S5 OCI run（app 200 + receipt fully_satisfied + K/D）/ S6 CI 同時 / S7 2 ブランチ非干渉 / S8 stop-resume / S9 SIGKILL 検知+回収 / S10 TTL・PR close / S11 分離負例（egress block・staging token 拒否・秘密非漏洩・GH runner 増加なし・cross-preview 403/404）/ S12 残留ゼロ。
- **KVM 必須を hard gate** とし、KVM 無し・dry-run・mock を PASS にしない。deployed identity（stack/golden/preview API/PWA asset）を前後で記録し、変化時は ENVIRONMENT-CHANGED。
- 現状は未実行。S6 は容量ポリシー決定まで `PARTIAL (CAPACITY)` 扱い。

## 8. Phase 5 Release / rollback（設計済み・ゲート BLOCKED）

- Manifest 追加案（PROPOSAL / NOT APPLIED）: preview runner の build id/commit、VM 管理 version（stack VERSION、provisioner/config/golden hashes、effective VM shape）、対象 K/D、実行 receipt、Static/Process/OCI E2E、CI/Preview 分離、cleanup、rollback readiness。
- rollback L0–L4: workload stop → runner registration 解除（`ato-preview-*` のみ）→ VM/overlay/volume 削除 → stack/config/golden 復元 → manifest/docs 戻し。golden/images/backups と `ato-ci-*` 登録は削除しない。
- **現在のリリースゲート: BLOCKED**（Preview Runner 未実装、容量・surface・validator・preview API secrets が未解決、#946 G1 未達）。

## 9. 未実施・要判断（次アクション）

1. 容量ポリシー（A / B / C）の選択と承認。
2. ホスト変更（br-pv/nft/dnsmasq/state root/systemd unit、golden role 追加）の承認と maintenance window。
3. preview API の D1 migration / secrets の適用範囲（operator 承認）。
4. public surface の方式（tunnel か公開 ingress か）。
5. validator companion の配置（preview 内か別ホストか）。
6. #946 の A1–A6 を preview runner で実測する計画（C 案なら CI idle 時）。

これらが決まるまで、VM 起動・ホスト変更・負荷試験は行わない。