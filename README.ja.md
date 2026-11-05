# Ato

**自分の計算環境を実行・保存・共有するためのメタランタイム。**

Atoは、自分が管理するコンピュータ上で、ソフトウェアやデータ、作業状態を実行・保存・共有し、続きを引き継ぐためのオープンソースのコンピューティングインターフェースです。

デスクトップPC、自宅サーバー、ワークステーション、クラウドVMなど、さまざまな計算環境で利用することを目指しています。このリポジトリでは、Atoの基盤ライブラリ、CLI、Runner、計算モデルの仕様を提供します。

Atoの特徴は、**ソフトウェアの実行だけでなく、計算そのものの保存と継続を扱うこと**です。

通常、アプリケーションを別の環境で動かすには、ソースコードや依存関係、実行環境を再構築し、必要なデータや設定を個別に復元する必要があります。しかし、同じアプリケーションを起動できても、以前の作業をそのまま継続できるとは限りません。

Atoは、これらを統一的なComputationモデルで扱います。実現すべき条件と、その条件を満たす実行方法を分離することで、特定のプログラミング言語、フレームワーク、ハードウェア、VM、コンテナ技術に依存しない計算の継続を目指します。

## ユースケース

Atoは、単純なアプリケーションの起動から、複数のソフトウェアやデータを組み合わせた作業の継続まで、幅広い用途を想定しています。

- **開発環境の共有** — チームメンバーが開発環境と作業状態を引き継ぎ、異なるマシンで開発を続ける。
- **アプリケーションのデプロイ** — ソフトウェアと必要な状態をまとめ、ローカル環境やリモートのRunnerで実行する。
- **AIエージェントのタスク引き継ぎ** — エージェントが利用したソフトウェア、生成したデータ、作業途中の状態を保存し、別の実行環境やエージェントへ引き継ぐ。
- **データと作業状態の復元** — ファイル、編集状態、操作履歴などを、それぞれに適した方法で再構成する。
- **複数アプリケーションの組み合わせ** — 異なるソフトウェアやデータを接続し、一連の作業として実行・保存する。

## Quick Start

Ato CLIを使って、いくつかのサンプルを実行できます。

これらのサンプルでは、単にアプリケーションを起動するだけでなく、**保存されたデータや作業状態から計算を継続すること**を目指しています。

### 1. インストール

Ato CLIはRustのCargo、または[GitHub Releases](https://github.com/ato-run/ato/releases)から入手できます。

```bash
cargo install --git https://github.com/ato-run/ato.git ato-cli --locked
```

### 2. 同梱の静的bundleで動作を確認する

まず、アカウント不要で現在の検証経路を確認できます。リポジトリをcloneし、同梱のbundleを実行します。

```bash
git clone https://github.com/ato-run/ato.git
cd ato
cargo build -p ato-cli --bin ato --locked
mkdir -p .tmp/getting-started
target/debug/ato run samples/interop-static.capsule --no-open \
  --verification-receipt .tmp/getting-started/receipt.json
```

`fully_satisfied: true` は、このbundleが宣言した観測がこのRunで満たされたことを意味します。一般的なホスト間移植性や状態の復元を実証するものではありません。

### 3. ホワイトボードの続きを編集する

保存されたCapsuleから、操作済みのホワイトボードを再開します。

```bash
ato run samples/whiteboard.capsule
```

ブラウザでホワイトボードが開き、あらかじめ配置された図形が表示されます。その状態から編集を続け、新しい継続地点として保存することを目指すサンプルです。

### 4. さらに試す

| サンプル | 体験できること |
|---|---|
| [Restore a Workspace](docs/guides/samples.md#restore-a-workspace) | ファイルや編集状態を復元し、作業を継続する。 |
| [Compose Applications](docs/guides/samples.md#compose-applications) | 複数のアプリケーションとデータを組み合わせて実行する。 |
| [Contract-preserving Derivation](docs/guides/samples.md#contract-preserving-derivation) | 同じKを維持しながら、異なるDで計算を実現・検証する。 |

それぞれのサンプルについて、対応環境、実行手順、検証できる条件を[サンプルガイド](docs/guides/samples.md)で説明します。

> **開発状況について：** ホワイトボードと複数アプリケーションのサンプルは構想段階です。上記のコマンドは目標とする操作例であり、現時点での動作を保証するものではありません。

## Atoを利用したサービス

Atoは特定のクラウドサービスに依存せず、ローカル環境や自分で管理する計算資源で利用することを目指すオープンソースプロジェクトです。

[ato.run](https://ato.run/)は、Atoを基盤として開発されている公式サービスです。ソフトウェアの発見・共有・実行を簡単にするため、DiscoverやマネージドRunnerなどの機能を提供しています。

Atoの利用にato.runのアカウントは必要ありません。

## ドキュメント

- [Getting Started](docs/guides/getting-started.md)
- [計算モデル](docs/concepts/computation.md)
- [Runnerとホスティング](docs/guides/runner-hosting.md)
- [サンプルガイド](docs/guides/samples.md)
- [技術仕様](docs/specifications/README.md)
- [ドキュメント一覧](docs/README.md)

## Contributing

Atoはオープンソースプロジェクトです。バグ報告、機能提案、実装への貢献を歓迎します。

- [GitHub Issues](https://github.com/ato-run/ato/issues)
- [GitHub Discussions](https://github.com/ato-run/ato/discussions)
- [Contributing Guide](CONTRIBUTING.md)

## License

Atoのプロジェクトコードは[Mozilla Public License 2.0](LICENSE)で公開されています。第三者のコードや個別のライセンスが指定された資料には、それぞれの条件が適用されます。

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
<a id="license"></a>
