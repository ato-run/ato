# Ato（日本語の概要）

Language: Japanese. 正式ドキュメントの基本言語は英語です。
[English README](README.md) と [ドキュメント目次](docs/README.md)を正本への入口として使用してください。

Atoは、計算のある地点を保存し、共有し、その先を続けるためのcomputing interfaceです。
中心となる **Computation** は、その地点から先に進める残余計算です。
**Capsule** はsealされた不変でaddressableな継続地点、**Run** はそこから進む評価体です。
Recordは観測された変化の証拠です。コンテナやVM、ファイルやSnapshotは実現方法であり、
すべてのCapsuleのidentityを定義するものではありません。

現在の主なproduct入口はWebサービスの **ato.run** です。このリポジトリはruntime、CLI、
Protocol、Adapterと関連ツールを含みます。CLIは開発者・automation向けの入口です。

| 読みたい内容 | 入口（英語） |
|---|---|
| Atoの思想 | [Computation](docs/concepts/computation.md) |
| Capsule / Contract K / Derivation D / Source | [Core Concepts](docs/concepts/application-contracts.md) |
| アーキテクチャ | [Architecture](docs/architecture/README.md) |
| 最初の実行・検証 | [Getting Started](docs/guides/getting-started.md) |
| 正式仕様・実装契約 | [Specifications](docs/specifications/README.md) |
| Formation | [Formation Guide](docs/guides/formation.md) |
| Runner / Hosting | [Runner Guide](docs/guides/runner-hosting.md) |
| 開発への参加 | [Contributing](CONTRIBUTING.md) |
| 運用・Rollback | [Operations](docs/operations/README.md) |

Computation-root profileは`ComputationRef`をidentityとしwire v2で運びます。
Portable application profileはContract Kの`ContractRef`をidentityとし、Derivation Dが
その実現方法を記述します。wire v3/v4を既存v2のrefへ読み替えません。
Bundle全体のSHA-256はtransportの同一性を示す値です。

互換性とはContract Kの維持と検証済みの再実現です。ただしKの同一性だけで、
既存データの継続、副作用の安全性、脆弱性修正や安全なrolloutまで保証しません。
[互換性](docs/concepts/compatibility.md)と[State Continuity](docs/concepts/state-continuity.md)を参照してください。

accepted RFC、Draft、実装、配備結果は別々に確認します。Draftに実装があっても、
正式仕様への昇格や本番での有効化を意味しません。過去の日本語の検証記録は、
言語を明示して[Archive](docs/archive/README.md)に保持しています。

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
