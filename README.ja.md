# Ato

Atoは参照可能な地点から作業を保存・共有・継続するためのコンピューティングインターフェースです。計算モデルと、Capsuleを扱うランタイム・CLIを組み合わせています。

[ato.run](https://ato.run/)は動作するソフトウェアの発見、試用、保存、共有のためにAto上に構築されたWebサービスです。このリポジトリにはランタイム、CLI、Adapter、関連ツールが含まれます。[About Ato](https://ato.run/about/) · [English](README.md)

## 解こうとしている問題

身近な人が自分のために作った小さな道具があるとします。その道具を自分のデータで使い、一か所だけ変更し、作者が離れた後も来週別のマシンで開きたい。現在のソフトウェアは通常、ソース、イメージ、セットアップ手順として共有されるため、受け取った側は作者がすでに到達した地点と、その周辺のデータやサービスの再構築が必要になります。

作業は多くの場合、プロセス、マシン、ソースツリー、特定のスナップショットに結び付いています。別の場所に移すには、環境を再構築し、結果が期待どおりに動くか確認しなければならないことがあります。Atoは作業の同一性と、それを実現するプロセス・コンテナ・スナップショットなどの物理的な手段を分けるアプローチを探究しています。

アプリケーションの共有、開発作業の継続、長時間のエージェントタスクの引き継ぎを、共通のインターフェースで扱うことを目指しています。これらはモデルのユースケースであり、すべての状態や引き継ぎが現在サポートされているという意味ではありません。

## Atoが検証していること

- 実現方法から独立した継続地点として、封印されたCapsuleは、Replay、ファイルシステムの再構成、チェックポイント、VMスナップショットのどれで実現しても同一性を保ちます。
- Contractで索引付けされた等価性として、バイト列が異なっても、同じ宣言済みContract Kを満たす2つの実現を同じ地点として扱えるか。これは仮説であり、現在の実装が任意の成果物について検査している動作ではありません。

どちらも新規性の主張ではありません。この組み合わせがソフトウェアを共有し継続するための実用的な単位になるかが開かれた問いです。[研究ツアー](docs/guides/research-tour.md)がモデル、既存の証拠、先行研究、意見を求めたい問いをつなぎます。より深い理論の記述は[理論ドラフト](docs/theory/capsule-process-model.md)にあります。

## 基本概念

- Computation：現在地点から継続可能な計算・作業。
- Capsule：封印された、不変で参照可能な継続地点。保存・共有でき、実現する物理的な方法はその同一性を定義しません。
- Run：Capsuleを再開して計算を進める実行。同じCapsuleから複数のRunを始め、それぞれ異なる未来へ進められます。

ポータブルアプリケーションでは、Contractが観測可能な要件を記述し、Derivationがその実現方法を宣言します。検証receiptはRunで得られた観測結果を記録します。同一性のrootは2つのprofile（`ComputationRef`と`ContractRef`）に分かれ、相互に読み替えてはいけません。詳細は[Computation](docs/concepts/computation.md)と[アプリケーション契約](docs/concepts/application-contracts.md)（profile対応表と未決事項を含む）を参照してください。

## クイックスタート

### CLIをローカルで試す（アカウント不要）

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

この静的な例は一般的なホスト間移植性やプロセスメモリの復元を実証するものではありません。

### ato.runを試す（任意）

ブラウザで[ato.run](https://ato.run/)の公開ソフトウェアを探すか、[2048を試せます](https://ato.run/?view=discover&detail=discover%3Adiscover-2048)。ato.runは別のプロダクト面であり、上記のローカル例に必要ではありません。

## 現在の状況

Atoは実験的なプロジェクトです。このリポジトリには計算オブジェクト、ライフサイクルコマンド、Adapter、ポータブルbundleの処理、上記の静的な検証例が含まれます。Python、OCI、GPU、状態、外部サービスには、それぞれ宣言された経路とホストでの実行許可が必要です。コードの存在はato.run本番での利用可能性を意味しません。

異なるホスト間の汎用的な復元、プロセスメモリの復元、AIによる自律的なFormationは研究・開発段階です。すべてのアプリがどこでも動く保証や、Docker・Nixなどのツールの代替を目的とするものではありません。Contractの一致だけでは、状態の継続や副作用の安全性は保証されません。[互換性](docs/concepts/compatibility.md)と[状態の継続性](docs/concepts/state-continuity.md)を参照してください。

## 研究と証拠

- [研究ツアー](docs/guides/research-tour.md)：問題、モデル、2つのprofile、データの同一性、検証済みの結果、独立性、未解決の問い。
- アーカイブされた実験：[contract-preserving transformation v0](docs/archive/2026/ops/contract-preserving-transformation-v0.md)と[stateful transformation v1](docs/archive/2026/ops/stateful-transformation-v1.md)。記録済みのfixtureと限界を持つLinuxでの歴史的証拠であり、この概要のために再測定したものではありません。
- [仕様](docs/specifications/README.md)：acceptedのcomputation-root RFCと、portable applicationの実装トラック。

## ロードマップと作業管理

作業は[GitHub Issues](https://github.com/ato-run/ato/issues)（ボードを使う場合はGitHub Projects）で管理します。このリポジトリにMarkdownのロードマップは置きません。[アーカイブ済みのロードマップ](docs/archive/2026/legacy/OPEN_ISSUES_ROADMAP.md)は履歴として残した資料で、更新しません。

## ドキュメント

一般的な利用者向けドキュメントは[ato.runのドキュメント](https://ato.run/docs/)が所有します。このリポジトリはランタイム、CLI、仕様、運用の技術ドキュメントを所有します（[ドキュメント目次](docs/README.md)）。

- [研究ツアー](docs/guides/research-tour.md)・[理論ドラフト](docs/theory/capsule-process-model.md)
- [Getting started](docs/guides/getting-started.md)・[Formationガイド](docs/guides/formation.md)
- [基本概念](docs/concepts/computation.md)・[アプリケーション契約](docs/concepts/application-contracts.md)・[用語集](docs/glossary-reference.md)
- [アーキテクチャ](docs/architecture/README.md)・[仕様](docs/specifications/README.md)
- [Runnerとホスティング](docs/guides/runner-hosting.md)・[運用](docs/operations/README.md)・[ドキュメント目次](docs/README.md)

## 貢献とライセンス

[貢献方法](CONTRIBUTING.md)と[GitHub issues](https://github.com/ato-run/ato/issues)を参照してください。

プロジェクトが作成したコードは[Mozilla Public License 2.0](LICENSE)（`MPL-2.0`）で提供します。第三者のコード、依存ライブラリ、上流コードを含むパッチ、独自のライセンス表示がある資料には、それぞれの条件が引き続き適用されます。例えば、CPythonのfixture、WiXテンプレート、リリースワークフローは上流のライセンスを、AODDスキルはMITライセンスを維持します。CLIの[Apacheライセンス本文](apps/cli/LICENSE-APACHE)はWiXテンプレートおよび過去に配布されたバージョンのために保持しています。過去に付与されたライセンスは引き続き有効です。

このコードのライセンスはCapsule Process Modelの理論文書に新たなライセンスを付与するものではありません。

| 対象 | 条件 |
|---|---|
| プロジェクト作成のコード | [MPL-2.0](LICENSE) |
| WiXテンプレートと過去配布版のために保持するCLIのApache本文 | [apps/cli/LICENSE-APACHE](apps/cli/LICENSE-APACHE) |
| 第三者のfixture・テンプレート・workflow・skill | それぞれの表示（元の場所に保持） |
| 理論ドラフトと仕様文書 | コードのライセンスは新たなライセンスを付与しない。元の条件に従う |

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
<a id="why-ato"></a>
