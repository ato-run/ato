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

   初回ビルドにはコンテナ・npm・Cargoの依存が必要です。`./prepare.sh` は指定コミットのGitオブジェクトだけを取り出し、validatorのトークンを初回に生成します。

   外部ネットワークのないゲストでは、ビルド端末でゲストのCPU向けに上記3つのイメージを作成し、`docker image save` で移送します。ゲストには同じ配布ディレクトリ、固定したRunnerバイナリ、Mailが指定するdigestのOCIイメージも用意し、Composeの3つのイメージを `docker image load` してください。ゲストでは次の手順から開始できます。`./start.sh` がローカルの秘密を初期化し、読み込み済みのイメージを起動します。

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
cp -R .tmp/mail-ca "$backup/"
```

Runnerではsystemdサービスの停止後、専用ユーザーの `~/.local/share/ato-selfhost`（work、volumes、identity）と `~/.config/ato-selfhost` をアクセス制限付きで別途保存します。同じデバイス資格情報を複製した2台を同時起動しないでください。

空の復元先で同一バージョンのイメージ、同じDNS名、保存済みの `.env` を用意します。`./init-runtime.sh` 実行後、保管した専用トークンを `.tmp/secrets/validator_token` に0444で配置し（親ディレクトリは0700）、保管したMailの公開CAを `.tmp/mail-ca/` に戻します。APIやCaddyを初めて起動する前に、**空の専用volume**に復元します。

```sh
./stack run --rm --no-deps -T --entrypoint tar api -C /var/lib/ato-api -xf - < "$backup/api.tar"
./stack run --rm --no-deps -T --entrypoint tar web -C /data -xf - < "$backup/tls.tar"
./start.sh
```

Runnerの同じユーザー・権限でデータと資格情報を戻し、元のホストを停止したうえでサービスを開始します。管理者ログイン、CAの指紋、Runner ID、Instanceの同じURL、実メールの保存内容を照合します。SQLiteだけを戻してオブジェクトやVolumeの復元を省くと、アプリの再開は確認できません。

## Mailの実機検証

[samples/ato-mail](../../samples/ato-mail/) のD1はlinux/amd64です。固定イメージと外部Binding（TLS、SMTP relay、固定TCPポート）、Instanceの永続Volume、別オリジンのRun Surfaceを専用ゲスト向けに用意する必要があります。[ブラウザ用サンプル](../../samples/ato-mail-browser/) はMissiveとStalwartで受信箱を表示する別の検証対象です。

内部CAでMailを試す場合は、検証用の公開CAをDER化し、`openssl dgst -sha256` で照合した `<64 hex>.der` を `.tmp/mail-ca/` に0644で配置します。validatorはここを読み取り専用で参照し、CLIで同じ条件を試すときは `ATO_TLS_ROOT_DIR` に同じ公開証明書ディレクトリを指定します。検証用の作業コピーの4つのTLS観測に `use = "ato.contract.tls-pinned-root@1"`、`root_ca_digest = "sha256:<64 hex>"` と検証用ホスト名を明記して、新しいbundleをsealします。CA秘密鍵は配布物・binding・validatorに含めません。[固定rootのTLS観測](../../docs/rfcs/draft/PINNED_TLS_ROOT_OBSERVATION.md)

管理者は、検証済みの同じRustリビジョンから `ato-egress-firewall-status` をビルドし、rootだけが変更できる `/usr/local/libexec/ato-egress-firewall-status` に配置します。配布の `ato-egress-firewall.service`、`ato-egress-firewall-status.socket`、`ato-egress-firewall-status@.service` をroot所有のsystem unitとして配置します。前者は管理者が有効化するboot時の設定で、ato専用bridgeの先頭2ルール（1080/TCPを許可、そのほかを拒否）を設定します。後者は引数や要求本文を受け付けず、起動時点のINPUTルールの検査結果だけを返します。Runner起動前に管理者は設定unitとstatus socketを有効化・起動します。Runnerユーザーとワークロードにsudo権限は必要ありません。Runner設定の `ATO_RUNNER_EGRESS_FIREWALL_MODE=verify-only` はこの読み取り専用socketで検査し、規則の欠落・順序違い・検査不能ならネットワーク実行を開始しません。

Mailの固定TCP listenerを割り当てる場合は、他サービスで使用していないゲスト側のIP・ポートを確認します。同居構成のvalidatorから到達できるcontrol bridgeのゲスト側アドレスを使用できます。Runnerの `runner.env` に `ATO_RUNTIME_NETWORK_CONTROLS=true` と `ATO_RUNTIME_FIXED_TCP_ALLOWLIST=<IP>:<port>,...` を指定し、heartbeatの固定TCP・egress能力を確認します。

StalwartでPROXY v2を使う場合は、`runner.env` の `ATO_RUNTIME_FIXED_TCP_PROXY_SOURCE` に固定listenerと同じゲスト側のIPを指定します。Stalwartの信頼するproxyにもその1アドレス（IPv4なら /32、IPv6なら /128）を設定します。RunnerはこのIPを送信元にして同じアドレス族のサービスへ接続します。省略時はOSが送信元を選択します。

所有者は対象Runnerを明示してInstanceを停止状態で作成します。運用者は `./stack run --rm --no-deps -T api configure-network INSTANCE_ID RUNNER_ID /run/policy.json` に、読み取り専用でマウントしたポリシーを渡します。ファイルをマウントする実行例です。

```sh
./stack run --rm --no-deps -T -v "$PWD/policy.json:/run/policy.json:ro" api \
  configure-network "$instance_id" "$runner_id" /run/policy.json
```

`fixed_tcp` の各要素はdeclared `port_id`、`bind_ip`、`port` を持ち、`egress` の各要素はdeclared `binding_id`、`destination_ipv4`、`ports` を持ちます。relay宛先・SNI・CAを試験用Dと揃え、メールの資格情報はInstanceのBindingとして設定します。`mail_data` に初期State Revisionを結合する場合は、v4のsaved-state bundleまたはホストの初期化経路が必要です。

初回の Mail 用に別途 Stalwart を起動し、[サンプルの設定手順](../../samples/ato-mail/README.md#staging-configuration-operator-steps)でテストドメイン、メールボックス、listener、Binding の環境変数参照を作成します。PROXY v2 を受ける listener は、実際に転送する Runner のアドレスを `/32` で登録します。初期化用の Stalwart を正常終了し、書込が止まったデータディレクトリと、同じ設定用に作ったアプリ単体の bundle を用意します。

次のコマンドを `ato` のルートで実行すると、既存の Rust exporter がデータを `mail_data` に結合して v4 bundle を作ります。`stopped_store` は作成した専用ストア、`mail_app` は元の bundle、`output` は未使用の出力先です。出力には指定ストア内のメールとアカウント情報が含まれるので、その所有者が管理します。TLS 私有鍵やリレーパスワードはストアの外に置き、起動時に Binding で渡します。

```sh
cargo run --locked -p ato-portable-application --example seed_filesystem_slot -- \
  "$mail_app" mail_data "$stopped_store" "$output"
```

1. Cloudflareと商用ato.runへの通信を遮断します。操作端末とRunnerはローカルDNS、管理用URL、アプリ用URL、テストに必要なMail endpointだけへ到達できるようにします。
2. 別端末から所有者としてログインし、Runnerを明示的に選択してMailのテストInstanceを起動します。検証receiptに同一bundle digest、ContractRef、選択Derivation、今回のattempt、`fully_satisfied=true` があることを確認します。
3. [メールボックス照合](acceptance/mailbox.py)を使って専用テストアカウントの下書きを保存します。アプリを保存・再開後、同じアプリURLから同じメールを確認します。
4. 専用ゲストを再起動し、同じオリジン・Runner ID・Instance・保存内容と新しい実行attemptを確認します。続けて空の環境へのバックアップ復元で同じ確認を行います。

このディレクトリの設定やHTTPのhealth応答だけでは、Mailの登録・Surface経由のログイン・保存・再起動復旧の結果は確定しません。
