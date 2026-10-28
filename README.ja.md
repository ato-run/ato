# Ato

Atoは、検証可能で移転可能なソフトウェアのためのオープンソースのランタイムと計算モデルです。

[ato.run](https://ato.run/)はAtoを利用して、動作するソフトウェアを発見・試用・保存・共有するプラットフォームです。このリポジトリにはランタイム、CLI、Adapterと計算モデルが含まれます。[About](https://ato.run/about/) · [English](README.md)

## できること

- [ato.run](https://ato.run/)で公開ソフトウェアを探して試す。
- CLIをビルドし、同梱のportable applicationを実行して検証receiptを確認する。
- 同じ契約を満たす複数の宣言された実行経路を検討する。利用可否はホストと選択した経路に依存する。

## 試す

[Discover](https://ato.run/)で公開アプリを探し、[2048](https://ato.run/?view=discover&detail=discover%3Adiscover-2048)を試せます。開発者向けのローカル例は、このcheckoutの指定Rust toolchainで実行します。

```sh
cargo build -p ato-cli --bin ato --locked
mkdir -p .tmp/getting-started
target/debug/ato run samples/interop-static.capsule --no-open \
  --verification-receipt .tmp/getting-started/receipt.json
```

[Getting Started](docs/guides/getting-started.md)にfixture、receipt、実行条件を記載しています。この例の成功は、一般的なホスト間移植性の証明ではありません。

## モデル

**Contract K**は実現が満たすべき観測可能な条件、**Derivation D**はその実現方法を記述します。**Capsule**はsealされた不変でaddressableな継続地点、**Run**はそこから進む評価体です。検証**receipt**は選択したDと実際の観測を記録する証拠です。

Computation-root profileは`ComputationRef`とwire v2、portable application profileは`ContractRef`とwire v3/v4を使います。ファイルのhashはtransport bytesの同一性であり、semantic identityではありません。Portable applicationの仕様は[実装段階のdraft](docs/specifications/README.md)です。詳細は[KとD](docs/concepts/application-contracts.md)、[Computation](docs/concepts/computation.md)、[計算理論](docs/theory/capsule-process-model.md)を参照してください。

## 現状と制約

Atoは実験的なプロジェクトです。このcheckoutには計算オブジェクト、ライフサイクルコマンド、Adapter、portable bundle処理と静的な検証例があります。Python、OCI、GPU、state、外部サービスにはそれぞれ宣言された条件とadmissionが必要で、コードの存在はato.run本番での利用可能性を意味しません。

異種ホスト間の汎用的な復元、process memoryの復元、AIによる自律的Formationは研究・開発段階です。すべてのアプリがどこでも動く保証や、Docker・Nixの代替を目的とするものではありません。Kの一致だけでデータの継続や副作用の安全性は保証されません。[互換性](docs/concepts/compatibility.md)と[State Continuity](docs/concepts/state-continuity.md)を参照してください。

## ドキュメントと連絡先

- [ドキュメント目次](docs/README.md)、[仕様](docs/specifications/README.md)、[Formation](docs/guides/formation.md)、[Runner](docs/guides/runner-hosting.md)
- [貢献方法](CONTRIBUTING.md)・[GitHub issues](https://github.com/ato-run/ato/issues)
- [ato.run Docs](https://ato.run/ja/docs/)・[お問い合わせ](https://ato.run/legal/contact/)

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

<!-- Additional Docsify slug aliases (including numeric prefixes). -->
<a id="capsule-run-replay-continue"></a>
<a id="core-kernel-と-adapter"></a>
<a id="設計原則：everything-can-be-a-capsule"></a>
