# 本番Worker ローリング更新 2026-11-02

## 対象と結果

対象は本番の8台のFirecracker VM（`ato-host-cpu-vm01..08`）。共有Runner `hetzner-ato-runner` と `ato-runner-agent.service`、ロールバック先の `ato-mail-stalwart` は変更していない。

- Worker: `327ece0208e5b912f2f8c158bada8eb6e3e0472e3590ff5a2f619bf222316ad1`
- ソース: `d35265f69796dde973c0ad0563c8e93c702ef100`。`main` の `b0b2219cf` との差分はテストコードのみ
- 8台すべてで稼働中。Worker、`/sbin/ato-init`（`d81e3baa091b`）、`ato-oci-start`（`ec26f30b777c`）のディスク上のダイジェストが一致

## 検出した問題

root実行のvm2でMemosが `oci_writable_user_unauthorized: ... observed 0:0` で失敗した。vm1だけが10/23からUID 1000で動いており、vm2〜8は旧initのrootのままだったため、1台目のカナリアでは検出できなかった。

vm2〜8の `/sbin/ato-init` と `ato-oci-start`（`--group=1000`）を、更新前のダイジェストを照合したうえでvm1と同じ内容にした。Runの作業領域は `/var/lib/ato-runtime-nonroot` に新規作成され、`/var/lib/ato-runtime` は保持している。

## 実Runの検証

| VM | アプリ | realization | 結果 |
|---|---|---|---|
| vm1 | Memos | oci | ready、停止確認済み |
| vm1 | openGym | oci_service_group | ready、停止確認済み |
| vm2（修正前） | Memos | oci | 失敗。head revision変更なし、grant aborted |
| vm2（修正後） | Memos | oci | ready、停止確認済み、head revision変更なし |

## 未検証

- vm3〜8はヘルスとheartbeatのみ確認。実Runは実施していない
- ログイン後のアプリ画面。app proxyの応答は `routed` と401まで
- process realizationの実Run
- Contract Receiptは2件とも `not_available`
- Staging E2EとRelease Manifest

## ロールバック

各VMの作業ファイルは `/var/lib/ato-host-cpu-control/.tmp/worker-rollout-20261102/slot-NN/` にある（`worker.before`、`rootfs.previous-*.ext4`、`result-*.json`）。受付後は古いディスク全体を戻さず、現在のディスクの複製でWorkerとinitだけを戻す。
