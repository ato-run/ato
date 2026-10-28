# Ato

Atoは、参照可能な地点から作業を保存・共有・継続するためのコンピューティングインターフェースです。計算モデルと、Capsuleを扱うランタイム・CLIを組み合わせています。

[ato.run](https://ato.run/)は、動作するソフトウェアの発見、試用、保存、共有のためにAto上に構築されたWebサービスです。このリポジトリにはランタイム、CLI、Adapter、関連ツールが含まれます。[About Ato](https://ato.run/about/) · [English](README.md)

## なぜAtoなのか

作業は多くの場合、プロセス、マシン、ソースツリー、特定のスナップショットに結び付いています。別の場所に移すには、環境を再構築し、結果が期待どおりに動くか確認しなければならないことがあります。Atoは、作業の同一性と、それを実現するプロセス・コンテナ・スナップショットなどの物理的な手段を分けるアプローチを探究しています。

アプリケーションの共有、開発作業の継続、長時間のエージェントタスクの引き継ぎを、共通のインターフェースで扱うことを目指しています。これらはモデルのユースケースであり、すべての状態や引き継ぎが現在サポートされているという意味ではありません。以下の例では、現在の静的アプリケーションの検証経路を試せます。

## 基本概念

- **Computation**：現在地点から継続可能な計算・作業。
- **Capsule**：封印された、不変で参照可能な継続地点。保存・共有でき、実現する物理的な方法はその同一性を定義しません。
- **Run**：Capsuleを再開して計算を進める実行。同じCapsuleから複数のRunを始め、それぞれ異なる未来へ進められます。

ポータブルアプリケーションでは、**Contract**が観測可能な要件を記述し、**Derivation**がその実現方法を宣言します。検証**receipt**は、Runで得られた観測結果を記録します。詳細は[Computation](docs/concepts/computation.md)と[アプリケーション契約](docs/concepts/application-contracts.md)を参照してください。

## クイックスタート

### ato.runを試す

ブラウザで[ato.run](https://ato.run/)の公開ソフトウェアを探すか、[2048を試せます](https://ato.run/?view=discover&detail=discover%3Adiscover-2048)。

### CLIを試す

[rust-toolchain.toml](rust-toolchain.toml)で指定されたRustツールチェーンをインストールし、リポジトリをcloneしてCLIをビルドし、同梱の静的アプリケーションbundleを実行します。

```sh
git clone https://github.com/ato-run/ato.git
cd ato
cargo build -p ato-cli --bin ato --locked
mkdir -p .tmp/getting-started
target/debug/ato run samples/interop-static.capsule --no-open \
  --verification-receipt .tmp/getting-started/receipt.json
```

CLIはbundleを検証し、宣言された実行経路で実現して、ソースの同一性とHTTP応答をContractに照らし合わせ、receiptを書き出します。成功後、`.tmp/getting-started/receipt.json`で選択されたDerivationと観測結果を確認できます。必要な観測がすべて満たされると`fully_satisfied`は`true`になります。`--no-open`はブラウザを開かずに検証して終了します。詳細、他の経路、失敗時の境界は[Getting started](docs/guides/getting-started.md)を参照してください。公開済みのCLI成果物は[GitHub Releases](https://github.com/ato-run/ato/releases)にあります。

例えば、Python 3でreceiptを確認できます。成功時の出力例は次のとおりです。

```sh
python3 -c 'import json; r=json.load(open(".tmp/getting-started/receipt.json")); print("fully_satisfied:", r["fully_satisfied"]); [print(o["id"] + ":", o["outcome"]) for o in r["observations"]]'
```

```text
fully_satisfied: True
app-proof: satisfied
source-identity: satisfied
```

この静的な例は、一般的なホスト間移植性やプロセスメモリの復元を実証するものではありません。

## 現在の状況

Atoは実験的なプロジェクトです。このリポジトリには計算オブジェクト、ライフサイクルコマンド、Adapter、ポータブルbundleの処理、上記の静的な検証例が含まれます。Python、OCI、GPU、状態、外部サービスには、それぞれ宣言された経路とホストでの実行許可が必要です。コードの存在はato.run本番での利用可能性を意味しません。

異なるホスト間の汎用的な復元、プロセスメモリの復元、AIによる自律的なFormationは研究・開発段階です。すべてのアプリがどこでも動く保証や、Docker・Nixなどのツールの代替を目的とするものではありません。Contractの一致だけでは、状態の継続や副作用の安全性は保証されません。[互換性](docs/concepts/compatibility.md)と[状態の継続性](docs/concepts/state-continuity.md)を参照してください。

## ドキュメント

- [Getting started](docs/guides/getting-started.md)・[Formationガイド](docs/guides/formation.md)
- [基本概念](docs/concepts/computation.md)・[アプリケーション契約](docs/concepts/application-contracts.md)・[用語集](docs/glossary-reference.md)
- [アーキテクチャ](docs/architecture/README.md)・[仕様](docs/specifications/README.md)
- [Runnerとホスティング](docs/guides/runner-hosting.md)・[運用](docs/operations/README.md)・[ドキュメント目次](docs/README.md)

## 貢献とライセンス

[貢献方法](CONTRIBUTING.md)と[GitHub issues](https://github.com/ato-run/ato/issues)を参照してください。

ルートにはリポジトリ全体に適用されるライセンスファイルがありません。一部のコンポーネントはmanifestでライセンスを宣言しています。例えば、[CLIのmanifest](apps/cli/Cargo.toml)は`Apache-2.0 OR MPL-2.0`を宣言し、[CLIのライセンスファイル](apps/cli/LICENSE)にはApache 2.0の本文が含まれます。対象コンポーネントのライセンス情報を確認してください。リポジトリ全体への許諾はここでは定められていません。

<!-- Compatibility anchors from the previous documentation entry. -->
<a id="ato"></a>
<a id="主なユースケース"></a>
<a id="設計原則everything-can-be-a-capsule"></a>
<a id="コアモデル"></a>
<a id="capsule--run--replay--continue"></a>
<a id="core--kernel-と-adapter"></a>
<a id="materialization"></a>
<a id="背景にある計算理論"></a>
<a id="基本ライフサイクル"></a>
<a id="capsuletoml-の例"></a>
<a id="プロジェクトのスコープ"></a>
<a id="現在の実装状態"></a>
<a id="capsule-run-replay-continue"></a>
<a id="core-kernel-と-adapter"></a>
<a id="設計原則：everything-can-be-a-capsule"></a>
