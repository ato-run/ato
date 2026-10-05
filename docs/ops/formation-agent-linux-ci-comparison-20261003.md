# Formation Session変更に伴うnative Linux CI比較

測定ID: `formation-agent-ci-linux-20261003-01`。2026-10-03の新しい測定であり、過去のFormation探索、実行pin、失敗Search、receipt、台帳を変更していない。

APIの`instance-state-sync`は、native Linuxでもexact base/headの両方で同じWebSocket handshake拒否を再現した。既存fixtureのRoom登録を修正したcommitでは、同じlockfile、ブラウザ、OS依存の全試験が通った。既存fixture不具合の比較であり、新しいSession変更の成功実測やCI全面成功には計上しない。

| Node | API pin | 結果 | 経過秒 |
|---|---|---|---:|
| 22.23.3 | base `02a9e58b` | FAIL: WebKit `coop-presence` handshake、`run.mjs:196` | 18.53 |
| 22.23.3 | head `9b483d4a` | 同じFAIL | 17.55 |
| 22.23.3 | fixture修正 `42d464d9` | PASS: 全browser fixture、12チェック | 49.96 |
| 25.6.0 | base `02a9e58b` | 同じFAIL | 16.61 |
| 25.6.0 | head `9b483d4a` | 同じFAIL | 16.33 |
| 25.6.0 | fixture修正 `42d464d9` | PASS: 全browser fixture、12チェック | 49.10 |

元のLinux CIログは`actions/setup-node`の`node-version: 22`と、実行版`v22.23.3`を記録している。Node 25.6.0は従来報告のローカル比較条件であり、元CIの条件ではない。上表のNode 25はその条件差を確認する追加比較。CIがcheckoutしたmerge `73d1da278406ca791539a75d6119e37e0bff66e7`のtreeはhead `9b483d4a`と一致する（`4ca2e04c7b0f43c0ab5acc55a5a53cb9402e4a1d`）。

## 原因と修正範囲

exact headの`src/app_proxy.ts`は、`COOP_PRESENCE_ROOM`がなければ503 `presence_unavailable`を返す。従来の試験workerは`AppRoom`だけをexportし、Miniflareにも`APP_ROOM`だけを登録していた。修正は既存の`CoopPresenceRoom`をfixtureへ登録するもの。WebSocketの検査やproduction認可を弱めていない。

Room登録後に進む認可試験では、認証済み非memberに対する期待値を既存実装の404 `not_found`へ合わせた。修正commitのheadからの差分は`tests/instance-state-sync/run.mjs`と`worker.ts`の2ファイルのみ。Node 22/25とも全試験が最後まで通り、ownerの複数tab・別context、Instance分離、grant、結果消失時の冪等retry、競合、reset fence、再接続、通知消失、古いGET、schema変更を確認した。

`nodeCrypto.randomBytes is not a function`のtelemetry警告はbase/head/修正後すべてで残る。警告の抑制、test skip、ブラウザエラーfilterは追加していない。

## 環境と再実行条件

`oci-linux-test`のUbuntu 24.04.4、Linux 6.17.0-1016-oracle、aarch64で実行した。GitHubのUbuntu 24.04.5/x64 runnerを再実行した結果ではない。専用ディレクトリは`/home/ubuntu/ato-run/.tmp/formation-agent-ci-linux-20261003-01`。既存checkout/cacheを変更せず、Git archiveから3つのSourceを展開した。全tracked blobを`git ls-tree`のdigestへ照合し、測定前・Node 22測定後・Node 25測定後ともbase 1810件、head/修正各1811件で不一致0。

Nodeは公式arm64 archiveと同versionの公式SHASUMSを照合した。pnpmは10.15.0、各Sourceは`pnpm install --frozen-lockfile --store-dir <専用pnpm-store>`で導入した。3つのlock SHA-256は`9555b0afe5dc40cdc06a8541735181d1dfea62a93f0e01aa6fd9149c6e42a20e`で一致する。Node 25の比較は同じnode_modules・storeを使用した。

Playwright 1.55.0の専用browserを使用し、事前launchでWebKit 26.0とChromium 140.0.7339.16を確認した。初回は不足するOSライブラリでlaunchに失敗したため、その準備失敗は上表に含めない。Ubuntuの8個のdebを`apt-get download`し、`dpkg-deb --extract`で専用prefixへ展開した。ホストへpackageをinstall/updateしていない。

ダウンロードしたWebKit launcherが`LD_LIBRARY_PATH`を上書きするため、専用browserの`minibrowser-wpe/sys/lib`へ展開済みライブラリのsymlinkを206件追加した。既存ファイルの置換、launcherや実行binaryの改変はしていない。全6条件で同じライブラリとbrowserを使った。測定後の専用ディレクトリは2.5GiB、ディスク空きは18GiB。

実行は各archiveのrootで次の環境を固定し、`/usr/bin/time -f 'elapsed_seconds=%e' pnpm test:instance-state-sync`を順次実行した。

```sh
formation_ci_root=/home/ubuntu/ato-run/.tmp/formation-agent-ci-linux-20261003-01
# 追加比較ではこの版だけを25.6.0へ変更する。
export PATH="$formation_ci_root/toolchain/node-v22.23.3-linux-arm64/bin:$formation_ci_root/toolchain/pnpm/node_modules/.bin:$PATH"
export TMPDIR="$formation_ci_root/scratch"
export PLAYWRIGHT_BROWSERS_PATH="$formation_ci_root/browsers"
export LD_LIBRARY_PATH="$formation_ci_root/os-deps/root/usr/lib/aarch64-linux-gnu"
```

## 証跡と未完了範囲

[機械可読の比較記録](evidence/formation-agent-linux-ci-comparison-20261003.json)に、全pin、各exit code・時間・log digest、公式Node archive digest、OSライブラリ、準備失敗の区別、Source照合結果を保存した。raw logはAPI worktreeの`.tmp/ci/linux-native-20261003-01/`へコピーし、元remoteの専用ディレクトリにも残した。

[前回の比較](formation-agent-ci-comparison-20261003.md)と[Windows追加検証](formation-agent-windows-ci-final-20261003.md)は当時の結果のまま維持する。元macOS CIのworker起動障害は、child log欠損のため原因未確定。Windows GNU cross-clippyの成功はnative Windows/MSVCの成功ではない。Activity/CORSの同一13失敗も未解決。新しいGitHub CIの再実行、Formation Search、実LLM、実Runtime attempt、private入力、配備、remote migration、flag変更は実施していない。追加有料API callは0。
