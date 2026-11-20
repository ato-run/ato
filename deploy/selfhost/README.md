# 1台のLinuxゲストでAtoをセルフホストする

この配布構成は、空のLinuxゲストでCoordinatorとWeb UIを起動し、所有者がブラウザから専用Runnerを登録するためのものです。検証用ドメイン、DB、アカウント、Runnerの作業領域を用意してください。Mailの実運用データを使う前に、独立したテストメールボックスで再起動と復元を確認します。

| 実行単位 | 配置とデータ |
|---|---|
| `web` | CaddyがHTTPSで静的PWAを配信し、同じオリジンの `/api/*`、`/v1/*`、`/health` をAPIへ転送。CA、証明書と鍵は `tls-data` に保存 |
| `api` | Node.js/Hono。SQLite・ローカルオブジェクト・認証用秘密は `api-data` に保存。LinuxゲストにAPIの生ポートを公開しない |
| `validator` | APIとネットワーク名前空間を共有する独立したRustプロセス。専用トークンでローカル検証用listenerに接続 |
| Connected Runner | 専用のLinuxユーザーで実行するsystemd user service。デバイス資格情報・回復ジャーナル・Instance Volumeはゲスト側に永続化 |

WebとAPIは、Runnerの登録前、またはRunnerを止めた状態でも起動できます。利用者はDesktopを必要としません。Caddyは管理用ホストとアプリ用ワイルドカードホストを分離します。アプリ用ホストではAPIが対象Instanceのアクセスを検証し、明示したRunner ingressに短命の署名付き要求を送ります。既定の内部ingressは `https://runner.<管理ホスト>:8443` です。Caddyは署名ヘッダーを保持し、ゲスト側のRunnerが署名を検証します。

## ビルドと初回起動

Linux、Docker Engine/Compose v2、Git、OpenSSL、Bashが必要です。Runnerを動かすユーザーにはRustのリポジトリ指定toolchain、Linuxユーザー名前空間・bubblewrap、必要なOCI実行環境を用意します。API/PWAの現在のソースを取得するにはそれぞれのリポジトリへのアクセス権が必要です。

1. `ato`、`ato-api`、`ato-pwa` を同じ親ディレクトリにクローンします。各クローンが、この配布に同梱された [versions.env](versions.env) のコミットを保持していることを確認してください。
2. `ato/deploy/selfhost/` で次を実行します。ソースの場所が異なる場合は `./prepare.sh /path/to/ato-api /path/to/ato-pwa` と指定します。

   ```sh
   cp .env.example .env
   ./prepare.sh
   ./stack build
   ```

   初回ビルドにはコンテナ・npm・Cargoの依存が必要です。切り離した環境には、事前に作成した3つのイメージ、固定Runnerのバイナリ、Mailのdigest指定イメージを移送します。`./prepare.sh` はローカルのGitオブジェクトからソースを取り出し、validatorのトークンを初回に生成します。ビルドコンテキストに作業中の変更や未追跡ファイルは入りません。

3. `.env` の `ATO_HOST` に所有者用DNS名、`ATO_BIND_IP` に専用ゲストのLANアドレスを設定します。管理用ホスト名と `*.apps.<管理ホスト>` を同じIPに解決するローカルDNSを設定します。既定の内部ingress名はComposeのDNS aliasに登録されます。既定のbindはloopbackです。標準443を使えない場合は `ATO_HTTPS_PORT` を変更し、URLにも同じポートを付けます。

   ```sh
   ./start.sh
   ./stack ps
   cp .tmp/trust/root.crt selfhost-root.crt
   openssl x509 -in selfhost-root.crt -noout -sha256 -fingerprint
   curl --fail --cacert selfhost-root.crt https://ato.home.arpa/health
   ```

   別端末にはこの公開CA証明書だけを転送し、指紋を照合してからOSまたはブラウザの信頼ストアに追加します。管理用ホストとポートは以降同じ値を使用します。

4. 最初の所有者をホスト端末のBashで作成します。パスワードは12〜128文字の1行です。既存の所有者がいる場合はコマンドが失敗し、データを保持します。

   ```bash
   read -rs -p 'Owner password: ' password
   echo >&2
   printf '%s\n' "$password" | ./stack run --rm --no-deps -T api create-owner owner@example.org
   unset password
   ```

   別端末のブラウザで `https://ato.home.arpa` を開き、所有者としてログインします。

## Runnerの登録と再接続

同じゲストではCaddyとRunnerの間に専用Unix socketを使用します。Runnerユーザーで `install -d -m 0711 "$HOME/.local/share/ato-selfhost/ingress"` を実行し、その絶対パスを `.env` の `ATO_RUNNER_INGRESS_DIR` に設定して `./start.sh` を再実行してください。Caddyコンテナにはこのディレクトリだけを読み取り専用でmountします。systemdのsocket proxyが各リクエストをloopback上の署名gateへ中継します。外部ホストのRunnerを追加する場合は、APIからそのホスト専用のTLS ingressへの経路を用意し、`ATO_RUNNER_INGRESS_ORIGIN` と `ATO_RUNNER_PUBLIC_BASE_URL` を揃えます。

同一ゲストでもRunner専用の非特権ユーザーで操作してください。次の例はLinux上のこのユーザーのホームにインストールします。別ホストへRunnerを増やす場合は同じソースと公開CAを移送し、新しい登録コードを使用します。

```bash
. ./versions.env
cargo install --locked --path ".tmp/src/$ATO_RUNNER_REV/apps/connected-realization-worker"
install -d -m 0700 "$HOME/.config/ato-selfhost" "$HOME/.config/systemd/user"
install -m 0644 selfhost-root.crt "$HOME/.config/ato-selfhost/root.crt"
(
  umask 077
  printf '%s\n' 'ATO_API_URL=https://ato.home.arpa' \
    "ATO_RUNNER_API_CA_FILE=$HOME/.config/ato-selfhost/root.crt" \
    'ATO_RUNNER_PUBLIC_BASE_URL=https://runner.ato.home.arpa:8443' \
    > "$HOME/.config/ato-selfhost/runner.env"
)
install -m 0644 ato-selfhost-runner.service ato-selfhost-ingress.{socket,service} "$HOME/.config/systemd/user/"
mkdir -p -m 0700 "$HOME/.local/share/ato-selfhost/"{work,volumes}
"$HOME/.cargo/bin/ato-connected-realization-worker" doctor \
  --work-root "$HOME/.local/share/ato-selfhost/work" \
  --state-volume-root "$HOME/.local/share/ato-selfhost/volumes" \
  --require-explicit-runtime-placement
```

`runner.env` はRunner所有者が作成する設定です。systemdとBashの双方で読めるリテラルの値と絶対パスを使用します。rootless Dockerを使う場合は、専用ユーザーの実際の `DOCKER_HOST` もここに設定してください。ユーザー名前空間、OCI、永続Volumeの可否は実際の診断と登録後のheartbeatで確認します。ワークロードにゲストのDockerソケット、CAの秘密鍵、Runnerの資格情報を渡さないでください。

所有者のブラウザで `/runners` から登録コードを発行します。ゲスト上で次を実行し、入力待ちになったらそのコードを貼り付けます。

```sh
./enroll-runner.sh
systemctl --user daemon-reload
systemctl --user enable --now ato-selfhost-ingress.socket ato-selfhost-runner.service
journalctl --user -u ato-selfhost-runner.service -n 20
```

ホストの管理者は、ログイン前から起動できるよう専用ユーザーのlingeringと、ゲスト起動時のDocker起動を設定します。登録後はデバイス鍵と資格情報を使って同じRunnerに再接続します。ブラウザでRunner ID、heartbeat、利用可能な実行方式と容量を確認してください。

## バックアップと復元

所有者はRunnerをDrainし、実行中のアプリを保存・停止して停止確認を待ちます。バックアップ中はRunnerとComposeサービスを停止します。下記は管理面のコールドバックアップ例です。バックアップ先は管理者だけが読める暗号化された領域を指定してください。

```sh
umask 077
backup=/secure/backup/ato-selfhost
mkdir -p "$backup"
./stack stop
./stack run --rm --no-deps -T --entrypoint tar api -C /var/lib/ato-api -cf - . > "$backup/api.tar"
./stack run --rm --no-deps -T --entrypoint tar web -C /data -cf - . > "$backup/tls.tar"
cp .env versions.env .tmp/secrets/validator_token "$backup/"
```

Runnerではsystemdサービスの停止後、専用ユーザーの `~/.local/share/ato-selfhost`（work、volumes、identity）と `~/.config/ato-selfhost` をアクセス制限付きで別途保存します。同じデバイス資格情報を複製した2台を同時起動しないでください。

空の復元先で同一バージョンを用意し、同じDNS名と保存済み `.env` を設定します。`./prepare.sh` 実行後、保管した専用トークンを `.tmp/secrets/validator_token` へ0444で配置します（親ディレクトリは0700）。APIやCaddyを初めて起動する前に、**空の専用volume**に復元します。

```sh
./stack run --rm --no-deps -T --entrypoint tar api -C /var/lib/ato-api -xf - < "$backup/api.tar"
./stack run --rm --no-deps -T --entrypoint tar web -C /data -xf - < "$backup/tls.tar"
./start.sh
```

Runnerの同じユーザー・権限でデータと資格情報を戻し、元のホストを停止したうえでサービスを開始します。管理者ログイン、CAの指紋、Runner ID、Instanceの同じURL、実メールの保存内容を照合します。SQLiteだけを戻してオブジェクトやVolumeの復元を省くと、アプリの再開は確認できません。

## Mailの実機検証

[samples/ato-mail](../../samples/ato-mail/) のD1はlinux/amd64です。固定イメージと外部Binding（TLS、SMTP relay、固定TCPポート）、Instanceの永続Volume、別オリジンのRun Surfaceを専用ゲスト向けに用意する必要があります。[ブラウザ用サンプル](../../samples/ato-mail-browser/) はMissiveとStalwartで受信箱を表示する別の検証対象です。

内部CAでMailを試す場合は、検証用の公開CAをDER化し、`openssl dgst -sha256` で照合した `<64 hex>.der` を `.tmp/mail-ca/` に0644で配置します。validatorはここを読み取り専用で参照し、CLIで同じ条件を試すときは `ATO_TLS_ROOT_DIR` に同じ公開証明書ディレクトリを指定します。検証用の作業コピーの4つのTLS観測に `use = "ato.contract.tls-pinned-root@1"`、`root_ca_digest = "sha256:<64 hex>"` と検証用ホスト名を明記して、新しいbundleをsealします。CA秘密鍵は配布物・binding・validatorに含めません。[固定rootのTLS観測](../../docs/rfcs/draft/PINNED_TLS_ROOT_OBSERVATION.md)

1. Cloudflareと商用ato.runへの通信を遮断します。操作端末とRunnerはローカルDNS、管理用URL、アプリ用URL、テストに必要なMail endpointだけへ到達できるようにします。
2. 別端末から所有者としてログインし、Runnerを明示的に選択してMailのテストInstanceを起動します。検証receiptに同一bundle digest、ContractRef、選択Derivation、今回のattempt、`fully_satisfied=true` があることを確認します。
3. [メールボックス照合](acceptance/mailbox.py)を使って専用テストアカウントの下書きを保存します。アプリを保存・再開後、同じアプリURLから同じメールを確認します。
4. 専用ゲストを再起動し、同じオリジン・Runner ID・Instance・保存内容と新しい実行attemptを確認します。続けて空の環境へのバックアップ復元で同じ確認を行います。

このディレクトリの設定やHTTPのhealth応答だけでは、Mailの登録・Surface経由のログイン・保存・再起動復旧の結果は確定しません。
