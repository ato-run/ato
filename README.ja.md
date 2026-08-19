# Ato

Ato（アト）は、ファイル・アプリケーション・実行状態・操作履歴を **Capsule** という共通単位として保存・共有・再開可能にするコンピューティングインターフェースです。単なるファイル共有ではなく、「計算の途中地点」をそのまま手渡し、受け取り手がそこから続きを始められる世界を目指しています。ファイル単体、アプリ付きファイル、作業状態・履歴を含む状態のすべてを「Capsule」という統一モデルで表現します。

*([English README](README.md))*

---

## 主なユースケース

- **バグ再現の効率化**: 再現手順やスクリーンショットを送る代わりに、エラーが発生している Workspace・Terminal・Browser の状態を Capsule として共有します。受け取った人は環境構築を挟まず、その地点から直接デバッグを開始できます。
- **AIエージェントとの協調**: コードの差分だけでなく、実行したコマンド、ファイル変更、テスト、ブラウザ操作などの履歴を Replay して確認できます。作業地点から人間が引き継ぐことも、別の AI エージェントに続きを任せることも可能です。
- **アプリケーション状態の共有**: アプリそのものだけでなく、「このモデルと設定でここまで組み上げた」という実際の利用状態を共有します。受け取り手はそこから Continue し、自分なりの変更を加えて別の未来へ分岐できます。
- **開発作業の復元・引き継ぎ**: 開発サーバーの起動状態、Terminal の作業ディレクトリ、Browser で開いている画面などをまるごと保存します。Human ↔ Human だけでなく、Human ↔ AI 間の作業引き継ぎにも対応します。

※これらに共通するのは、「何を使ったか」だけでなく「どこまで計算が進んだか」を共有対象にする点です。

---

## 設計原則：Everything can be a Capsule

Atoでは対象ごとに別々の共有モデルを作るのではなく、含まれる要素に応じて共通の Capsule インターフェースへ射影（マッピング）します。

| Capsule に含める要素 | 可能な操作 |
| --- | --- |
| データ | Pass / Open |
| データ + アプリケーション | Open |
| データ + アプリケーション + 実行状態 + 履歴 | Replay / Continue |

*Everything can be a Capsule* は「すべてを同じデータとして扱う」という意味ではありません。性質の異なるコンピューティング対象であっても、保存・受け渡し・再開・合成という共通の操作で扱えるようにする設計原則です。

---

## コアモデル

### Capsule / Run / Replay / Continue

- **Capsule**: ある地点から続きを始められる計算を切り出した、Immutable（不変）かつ Addressable（参照可能）な値です。1つの Capsule から複数の Run を開始して異なる未来へ分岐できます。
- **Run**: Capsule から再開された、現在進行中の可変な計算状態です。Run を進めて保存すると新しい Capsule になります。
- **Replay**: 保存された Record を使い、そこまでの interaction を再生・適用します。「どうやってここまで来たか」を確認・再構築する操作です。
- **Continue**: Capsule の地点を再実現し、新しい Run を開始します。「ここから続きをやる」ための操作です。

```text
C41 ─────▶ C42 ─────▶ C43
            │
           seal
            ▼
       Capsule C42
          /     \
         ▼       ▼
       Run A    Run B
         │       │
        C43a    C43b
```

### Core / Kernel と Adapter

Atoは、計算の論理的な意味と、現実世界での実行方法を分離します。

| 要素 | 役割 | 扱う対象 |
| --- | --- | --- |
| **Core / Kernel** | 計算の identity・interaction・evolution を定義 | Computation, Port, Protocol, Evolution, Composition |
| **Adapter** | 論理的な計算と現実の I/O を接続 | Process, PTY, Workspace, HTTP, Binding など |

Core は「この計算が何であり、どう変化し、どう組み合わさるか」を扱います。Adapter はそれを OS プロセス、Terminal、ファイルシステム、HTTP などの具体的な世界へ接続し、interaction を Record として観測・適用します。

```text
               Computation
                    │
                Protocol
                    │
          ┌─────────┼─────────┐
          ▼         ▼         ▼
       Process     PTY     Workspace
       Adapter   Adapter     Adapter
          │         │         │
          └─────────┼─────────┘
                    ▼
                Physical world
```

この分離により、Web、Terminal、AI エージェント、あるいは将来的な新しい runtime を追加しても、Capsule そのものの意味を変更せずに拡張できます。

---

## Materialization

Atoでは、「どの計算地点を保存したか」と「その地点へ物理的にどう戻るか」を明確に分離します。前者が **Capsule**、後者が **Materialization** です。

```text
                    Capsule C42
                         │
          ┌──────────────┼──────────────┐
          ▼              ▼              ▼
       Replay       Filesystem     VM checkpoint
          │              │              │
          └──────────────┼──────────────┘
                         ▼
                      C42相当
```

同じ Capsule であっても、Replay、Filesystem reconstruction、Process checkpoint、VM snapshot など異なる方法で再実現（復元）できることを目指します。Snapshot や Container 自体を Capsule の identity に置かないことで、物理的な実行方法を交換可能にしています。

---

## 背景にある計算理論

Atoのモデルは、既存の計算理論やシステム研究に影響を受けています。これらを新しく発明するのではなく、計算を保存・受け渡し・継続するための systems model として再構成することを目指しています。

- **λ-calculus / Continuation**: 過去の処理そのものではなく、「現在地点から何が残っているか（Residual Computation / Continuation）」という捉え方。
- **π-calculus**: 計算を閉じた処理ではなく、Port を通じて他計算や外部世界と相互作用する Process として捉え、複数の Computation を合成（Composition）する考え方。
- **Kell Calculus**: 計算に明示的な boundary（境界）を与え、実行中の bounded process を Passivation（停止・抽出）する考え方。Atoの Capture / Seal に対応します。
- **Reversible Process Calculi**: 履歴を単なる一本道ではなく因果関係として扱い、Replay・Rewind・Fork を安全に行うための基礎。
- **Distributed Snapshot**: 複数 process や host で構成された Computation を、内部通信や in-flight message まで含めて一貫した地点として Capture するための基礎。

```text
Computation ──▶ capture / seal ──▶ Capsule ──▶ transfer ──▶ 別のruntime ──▶ materialize ──▶ Run ──▶ Continue
```

---

## 基本ライフサイクル

```sh
# 新しいlineageを作り、記録を開始
ato init demo

# 現在地点を保存
ato stop demo

# 保存地点から再開
ato resume demo@main

# 過去地点から新しいbranchを作る
ato resume demo@main#42 --branch experiment

# 1地点をportable Capsuleとしてexport
ato encap demo@main \
  --materialize ato.replay@1 \
  -o demo.capsule

# portable Capsuleを実行
ato run demo.capsule
```

### `capsule.toml` の例

現在の authoring では、開始する process や利用する Adapter を明示します。

```toml
schema = 1

[[process]]
id = "app"
command = ["python", "app.py"]
cwd = "."

[[adapter]]
target = "app"
use = "ato.process@1"

[[adapter]]
target = "workspace"
use = "ato.workspace@1"

[encap]
materializers = ["ato.replay@1"]
```

---

## プロジェクトのスコープ

**Atoが扱うもの**

- Computation / Capsule / Run の identity
- Capsule lineage と branch
- Port / Protocol による interaction
- Computation の Composition
- Adapter による Record / Replay
- Capsule の portable encoding
- Materialization による再実現

**Atoが目指さないもの**

- Docker / Nix / VM の代替となる万能な環境構築システム
- Package manager / toolchain provisioning の全面的な再実装
- Git や Container registry の代替
- あらゆる process の完全決定論的 Replay
- 万能な sandbox / orchestration system

※ Docker、Nix、VM、既存の runtime や AI エージェントを利用して計算環境を準備し、Atoはその上で進む計算を「記録・保存・分岐・転送・継続」可能にする層を担当します。

---

## 現在の実装状態

**実装済み**

- Immutable な ComputationObject と content-addressed な ComputationRef
- Port / Protocol による Computation evolution
- Computation composition
- Capsule lineage / branch / Run / Record
- CLI コマンド（`init`, `stop`, `resume`, `encap`, `run`）
- 各種 Adapter（Process, PTY, Workspace, HTTP, Binding）
- Portable `.capsule` bundle v2
- Protocol-generic な Replay Materializer

**実験的・開発中**

- Filesystem / workspace snapshot の physical restore
- Process checkpoint や VM snapshot を含む異種 Materialization
- 異なる host 間での汎用的な Resume
- 複数 host にまたがる Distributed Capture
- 異なる Materialization 間での Contract-equivalent realization

Atoは現在実験的なプロジェクトです。既存の全モデルが完成しているわけではなく、まずは異種の Computation を統一的な Lifecycle で扱えるか、段階的な検証を進めています。
