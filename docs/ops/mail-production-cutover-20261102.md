# 本番メールの切り替え 2026-11-02

`mail.ato.run` と `atousercontent.com` のメールを、`ato-mail-stalwart` コンテナから本番ライブラリのアプリ「Ato Mail」へ移した。オーナーの承認（2026-11-02）に基づく。

## 構成

| 項目 | 値 |
|---|---|
| Instance | `cinst_01M6DY1753PEAB7VY8WMBYACDF`（`cinst-xly5jcp2i2lnihzj.app.ato.run`） |
| Bundle | `bnd_01M6DY01SVFP93C4R4HSTYYD9G`（`sha256:14599f2c…`） |
| ContractRef | `sha256:ebbb644d…` |
| DerivationRef | `sha256:e5e8bf71…`（Stalwart、Adapter、Missive） |
| Runner | `rnr_mail0168ACB526134105F7AB19`、`ato-mail-runner.service`、uid 1000、1スロット、プール `hetzner-cpu` |
| Worker | `327ece0208e5…` |
| Validator | `3bb9324745df…` |
| API | `320baa3c`、version `9ee5a633` |
| 固定TCP | `65.109.37.38` の 25、465、587、993 |
| 状態 | Runnerボリューム `svol_01M6DX2C6BNC8VS52MJDBSWZNX`、スロット `isslot_mail_data_20261102` |
| 送信 | Postfix `ato-mail-relay.service`、`65.109.37.38:10587`（ループバック以外をiptablesで拒否）、egressはこの宛先のみ |
| Runner入口 | `https://mail.ato.run`（Caddy） |
| 実行方針 | `always_on` / `running` |
| ID・パスワード | `test@atousercontent.com`、パスワードは `/srv/ato-mail/test-atousercontent.password`（root専用） |

Capsuleの作成元はサンプルを本番値に置き換えたもので、リポジトリには含まない。本番値はリレーの宛先、SNI、CA、Kの `server_name=mail.ato.run` で、秘密情報用のBindingは削除している。

## 計画からの変更

| 計画 | 実施 | 理由 |
|---|---|---|
| `user_managed` の専用Runner、明示配置 | `ato_managed`、`hetzner-cpu` プール | 固定TCPは `ato_managed` だけに割り当てられ、managed配置は基準プールしか選ばない |
| 証明書とリレー認証をBindingで渡す | Stalwartのストア内に保持 | Bindingの投入にはオーナーのセッションが必要で、MCP経由では秘密情報が会話に残る |
| IPv6のテストドメイン | 本番ホストの高番号ポートで検証 | APIの `bind_ip` は公開IPv4のみ。DNS変更も権限で拒否された |
| Stagingの総合検証 | 未実施 | 本番ホストの高番号ポートでContract `verified` を確認して代替した |
| `mail-01.runner.ato.run` | `mail.ato.run` を流用 | DNS変更が権限で拒否された |

## 検証

- 高番号ポートのRun `run_01M6DY22D09Y23M8XTN59JT41W`: `verification_status = verified`
- 切り替え後のRun `run_01M6DY6GRFFHFWKAKWSWT9BHPA`: `ready`
- 公開ポート: 587からのSMTP AUTH送信、993のIMAPログイン、25のSTARTTLS受信（本番ホストからの接続）
- 外部送信: GmailのMXが250で受理。受信箱での表示、SPF、DKIMの判定は未確認
- 外部からの受信: 第三者MTAからの実配送は未確認。check-hostは1ノードのみ接続成功
- Webクライアント: コンテナ内の `/health` と `/login` は200。app URLからのログイン後の画面は未確認

## 影響

- 切り替え中、25、465、587、993は約2分停止した
- リレーの設定不備で、最初の外部テストメールは約2分遅れた
- 受信経路のテストでリレーがループを検知し、バウンス1通を `postmaster@ato.run` へ送った

## 既知の問題

- 証明書の自動更新が止まっている。`ato-mail-cert-sync.timer` は切り戻し用コンテナ向けなので無効化した。現在の証明書は2027-01-30まで有効で、Caddyの更新後にストアへ手動で反映する必要がある
- プール `hetzner-cpu` に所属するため、このRunが止まっている間は他のmanaged Runが載る可能性がある
- `management_pool_id` の選択を実装するまで、この構成は専用の隔離を保証しない
- 旧インスタンス `cinst_01M6DX2YPAXAJXVD1D54CQ85QN` の削除はMCPが403で失敗し、ライブラリに残っている
- ボリュームのバックアップは未設定

## 切り戻し

1. Ato Mailを停止し、リースが `stopped` になるのを確認する。
2. 本番ホストで `/root/ato-mail/rollback-to-container.sh` を実行する。

切り替え後に受信したメールはボリューム側に残り、コンテナへは戻らない。切り替え直前のコンテナのデータは `/srv/ato-mail/data` と `/root/ato-mail/rollback-data-20261102.tgz` にある。
