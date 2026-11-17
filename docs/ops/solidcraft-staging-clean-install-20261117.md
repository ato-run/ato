# SolidCraft — staging 配布 CLI のクリーンインストール検証（2026-11-17）

Status: Archived — v0.9.3 検証時点では **不合格**。以下は修正前の履歴として保持する。
検証時刻: 2026-11-17 16:27 JST。

後続の [0.9.4-stg 受け入れ](solidcraft-staging-094-acceptance-20261117.md) では配布経路を修正し、
公開 installer から新規環境で D1・D2・D2 再実行の cache hit まで成功した。
この結果は以下の v0.9.3 の失敗を取り消すものではない。

## 条件

既存の M1/macOS arm64 ホスト上に、新しい HOME、ATO_HOME、XDG config/cache、Cargo/Rustup home、
インストール先、TMPDIR を用意した。PATH は新規 bin と OS 標準のディレクトリに限定し、
既存の CLI、認証情報、Rust toolchain、Cargo registry、native build cache は再利用していない。
OS 自体を初期化した検証ではない。

配信中の [staging install.sh](https://stg-app.ato.run/install.sh) を保存し、変更せず
`sh install.sh --cli-only --no-modify-path` で実行した。release/version の上書きや
`ATO_SKIP_NACELLE_INSTALL` は設定していない。
この installer は staging branch の成果物ではなく、`ato-run/ato` の GitHub latest release を解決する。
実際に取得されたのは [v0.9.3](https://github.com/ato-run/ato/releases/tag/v0.9.3)
（source `8b058e52f1a4d93e541cb863691266736122c632`、公開 2026-11-16 08:09:59 UTC）。
`ato --version` の実測も `ato 0.9.3`。

## 結果

| 段階 | 結果 |
| --- | --- |
| インストール | CLI 本体の配置後、nacelle の取得で HTTP 404。archive と legacy 3 候補がすべて失敗し、135.9 秒で exit 1 |
| 実行方法一覧 | `--list-derivations` が未対応、exit 2 |
| D1 | staging の公開 bundle を取得・digest 一致後、`ato.derivation/1` の reference extraction で `missing field probe_port`、exit 1 |
| D2 | `--allow-native-source-build` が未対応、argument parse で exit 2 |
| receipt | D1/D2 とも生成されず、native process 起動・source build には到達しない |

nacelle 取得中に、すでに配置された同じ配布版 CLI で D1/D2 を実行した。
インストーラー全体の成功を前提にした結果ではない。両失敗は nacelle 起動より前に発生する。
D2 は既承認の `--allow-native-source-build --allow-unsigned-native` を明示した。

```sh
ato run capsules/ato/solidcraft --api-url https://staging.api.ato.run \
  --derivation sha256:f58cdca4697158f519a9a24d7bc36a7deb00a531b913fce3859deccbb46551e9 \
  --no-open --verification-receipt d1-receipt.json

ato run capsules/ato/solidcraft --api-url https://staging.api.ato.run \
  --derivation sha256:382f1474eb77e0c18820f65a4d96edebfeddda6cc6b2b1cb5f4834bb9462406d \
  --no-open --allow-native-source-build --allow-unsigned-native \
  --verification-receipt d2-receipt.json
```

取得 bundle は `sha256:35f940038bc66cec78eb53485738bf029d4ce68d619993a024f8a3a10c10c7e3`。
検証前後で release と公開 bundle は同じで、staging run_count は 3 のまま。
新しい成功 receipt の報告、公開物の更新、staging deploy、本番操作は行っていない。

## 前回の受け入れとの関係

[前回の M1 受け入れ](capsule-hub-m2-acceptance-20261116.md) は修正を含むローカルビルド CLI
（runtime source `bdd62dace9b6c1c295ad3a3baa3563d18e239139`）での成功。
今回の配布版 v0.9.3 はその native terminal/source build 対応と修正を含まない。
そのため、前回の成功をクリーンインストールからの成功に拡張できない。

この経路を成立させるには、インストーラーと実際の release assets の整合性を直し、
native terminal/source build 対応を含む CLI を staging から取得できるようにした上で、
同じクリーンインストール検証を再実行する必要がある。この検証では配布経路を変更していない。

## 証跡

[機械可読サマリ](evidence/solidcraft-staging-clean-install-20261117.json) に installer/binary/bundle の
SHA-256、release source、exit code、receipt 未生成と検証範囲を記録した。
生ログ・配信 installer・取得 bundle・隔離環境は workspace の `.tmp/sc-clean-ukogw4x1/`。
`install.log`、`d1.log`、`d2.log`、`run-help.log`、`list-derivations.log`、各 `*-result.json` を保存する。
