# 再現可能ビルドを超えて: クロスプラットフォームなソースネイティブソフトウェアのための Execution Identity

## Abstract

再現可能ビルドは、ソフトウェア成果物を説明可能なものにした。同じソースコード、ビルド環境、ビルド手順が与えられれば、誰でも指定された成果物とビット単位で一致するコピーを再生成できるべきだ、という考え方である。この枠組みは、コンパイラ、OS ディストリビューション、パッケージエコシステム、サプライチェーン検証において大きな成功を収めてきた。しかし、クロスプラットフォームなソースネイティブソフトウェア配布では、別の対象を同定する必要がある。すなわち、ビルドが生み出す成果物だけでなく、ソースコードが実行中のプロセスへと変わるときの起動条件である。

あるソースツリーは再現可能にビルドできても、プラットフォームごとに異なる形で実行されうる。原因は、ランタイム解決、依存物の materialization、動的ライブラリ、環境変数の漏洩、filesystem view、ネットワークポリシー、永続状態、ロケール、タイムゾーン、エントリポイント設定などにある。既存システムは隣接する対象を識別している。Reproducible Builds は artifact bytes を識別する。Nix は derivations と store outputs を識別する。Docker は images を識別する。パッケージマネージャは dependency resolutions を識別する。ReproZip は事後的な execution trace を捕捉する。だが、これらはいずれも、ソースネイティブソフトウェアの pre-execution launch envelope を直接には識別しない。

本論文は、ソースネイティブソフトウェアの launch conditions を内容アドレス化した表現である **Execution Identity** を導入する。Execution Identity は、source tree identity、dependency derivation identity、dependency output identity、runtime identity、environment closure、filesystem view、network policy、capability policy、entrypoint、arguments、working directory をハッシュ化する。さらに本論文は、`pure`、`host-bound`、`state-bound`、`time-bound`、`network-bound`、`best-effort` という **reproducibility classes** を導入し、クロスプラットフォーム実行の drift がなぜ生じるのかを明示する。

本モデルは、ローカルプロジェクト、GitHub リポジトリ、共有アプリケーションハンドルのためのソースネイティブ実行ランタイムである Ato に実装されている。Ato は、source provenance、materialized source identity、dependency derivation、immutable output blobs、session-local effects、persistent user state を分離する。私たちは、Execution Identity によって drift detection、launch-envelope replay、非汚染実行、監査可能なクロスプラットフォームのソース配布が可能になることを示す。

---

## 1. Introduction

ソースコードは、もはや単にビルドするものではなく、実行するものとして配布されることが増えている。

開発者は、完全なセットアップ手順を読む前にリポジトリを clone して demo を起動する。ツールは GitHub から直接呼び出される。スクリプトはローカルワークフローの一部として生成され、実行される。AI コーディングエージェントは、単一タスクの間にソースリポジトリを調べ、変更し、実行し、破棄する。科学計算やデータワークフローも、事前ビルド済みバイナリだけでなくソースツリーとして共有される。このいずれの場合も、ユーザーの期待は似ている。

```text
Given this source, run the same thing here.
```

しかし、「同じ thing」とは何かを定義するのは難しい。

これに対する確立された答えが再現可能ビルドである。ビルドが再現可能であるとは、同じソースコード、ビルド環境、ビルド手順が与えられたとき、誰でも指定されたすべての成果物とビット単位で一致するコピーを再生成できることを指す。reproducible-builds.org の定義も、artifact identity を中心に据え、通常は暗号学的ハッシュによるビット単位比較で再現性を検証することを明示している。

これは必要だが、それだけでは不十分である。

再現可能ビルドが識別するのは artifact である。一方、ソースネイティブ実行が行うのは process の起動である。プロセスが観測するのは起動時の world だ。PATH やツールマネージャによって選ばれる runtime binary、パッケージマネージャによって materialize された dependency tree、継承または再構成された環境変数、動的ライブラリ、filesystem mounts、書き込み可能な state、ネットワーク制約、locale、timezone、command-line arguments、working directory がそこに含まれる。

同じ source tree を持つ 2 台のマシンでも、異なる実行を起動しうる。同じ Docker image digest を持つ 2 人のユーザーでも、bind mounts、環境変数、コマンド、working directory、network options が違えば異なる実行を起動しうる。Docker 自身も、`docker run` を options、image reference、任意の command と arguments からなる合成として定義しており、command、entrypoint、environment variables、user、working directory などの image defaults が実行時に override されうることを文書化している。

同様に、Nix は derivations と store outputs に対する厳密なモデルを提供する。Nix derivation は、system、builder、arguments、environment variables、inputs、outputs といった属性で記述される build task である。しかし、source-native run は derivation output だけではない。それは launch である。すなわち、選択された runtime、dependency projection、filesystem view、environment closure、policy、entrypoint、arguments、working directory、そして場合によっては state binding である。

本論文は、クロスプラットフォームなソースネイティブソフトウェア配布には、新しい identity object が必要だと主張する。それが **Execution Identity** である。

目標は、すべてのプラットフォームで完全に同一の process trace を保証することではない。それは deterministic record/replay の問題へと崩れ、別問題になる。本論文の目標は、execution launch を説明可能なものにすることだ。

```text
Build reproducibility identifies artifacts.
Execution Identity identifies launches.
```

2 つの execution が同じ execution identity を持つとき、それらは等価な launch conditions を持つ。等価な launch conditions が等価な振る舞いを生むかどうかは、その execution の reproducibility class に依存する。

本論文の貢献は 3 つある。

1. **Execution Identity.** ソースネイティブソフトウェアにおける pre-execution launch envelope の content-addressed identity を定義する。
2. **Reproducibility Classes.** 再現性を二値的に扱うのではなく、非再現性の原因によって execution を分類する。
3. **Ato Reference Runtime.** layered state、dependency materialization、runtime identity、environment closure、filesystem views、policy hashes を通じて execution identity を計算・記録・再構成する source-native runtime として Ato を記述する。

---

## 2. Why Build Reproducibility Is Not Execution Reproducibility

### 2.1 Reproducible builds identify artifacts

再現可能ビルドが問うのは次のことだ。

```text
Can the same source, build environment, and build instructions recreate the same artifact bytes?
```

出力対象は artifact である。実行ファイル、パッケージ、配布アーカイブ、filesystem image、あるいは他の指定された build result がそれにあたる。成功条件はビット単位の一致である。

このモデルが強力なのは、artifacts を説明可能にするからである。独立検証、サプライチェーン透明性、配布の信頼性、ビルド非決定性のデバッグを支える。

しかし、artifacts は executions ではない。プロセス起動は、artifact 自体の外部にある多くの条件に依存しうる。

### 2.2 Hermetic builds identify build conditions

Hermetic build systems は、ビルドをホストから隔離することでさらに先へ進む。Bazel は hermetic build を、同じ入力ソースコードと product configuration が与えられれば、ビルドをホストシステムの変化から隔離することで常に同じ出力を返すものと説明している。そこではツールも管理された inputs として扱われ、ビルドツールやライブラリの特定バージョンに依存する。

これは近い考え方だが、依然として build 中心である。identity target は build action とその output だ。Execution Identity は、同じ規律を process launch conditions へと移す。

```text
Hermetic builds make build actions accountable.
Execution Identity makes launch actions accountable.
```

### 2.3 Nix identifies derivations and store outputs

Nix は、本研究にとって最も強い対比対象である。Nix はすでに、build derivations、input closure、immutable store outputs、クロスプラットフォームな package construction に対する厳密なモデルを提供している。derivation は、system type、builder、arguments、inputs、environment variables、outputs を含む build task を記述する。

本論文は、Nix がその本来の領域において「不安定」あるいは「不十分」だと主張すべきではない。より適切なのは次の言い方である。

```text
Nix makes build inputs explicit.
Execution Identity makes launch conditions explicit.
```

違いは identity target にある。

Nix store path は package output を識別できる。しかし、それだけでは任意の source-native launch における次の要素を識別できない。

```text
runtime binary
dependency projection
environment closure
filesystem view
network policy
capability policy
entrypoint
argv
cwd
state bindings
```

Nix はこれらの多くを構築するために使える。しかし、launch envelope それ自体は Nix における primary identity object ではない。

### 2.3.1 Nix in Practice: Launch Drift After Store Identity

この違いは理論上のものにとどまらない。実際の Nix 運用では、derivation model の失敗というより、launch-envelope drift として捉えた方が適切な問題群が観測される。

たとえば、interactive な Nix 環境は、必ずしも完全に閉じた execution environment を形成しない。[Nix manual](https://nix.dev/manual/nix/latest/command-ref/nix-shell.html) は、`nix-shell --pure` が環境を「ほぼ完全に」消去するとしつつ、`HOME`、`USER`、`DISPLAY` のような変数は保持し、さらに shell resolution が `NIX_BUILD_SHELL` や `NIX_PATH` の影響を受けうることを説明している。実際、Nix で build された terminal emulator から子 shell に `PATH`、`LD_LIBRARY_PATH`、`PYTHONNOUSERSITE` のような wrapper-provided variables が伝播する事例や、ユーザーの R library path が Nix の R development shell に漏れ込む事例も報告されている。これらは derivation output の問題ではなく、environment closure の問題である。

動的ライブラリ解決も別の例を提供する。[nix-ld README](https://github.com/nix-community/nix-ld/blob/main/README.md) は、`LD_LIBRARY_PATH` がすべてのプログラムに影響し、RPATH を持つ正しく build された Nix application にも誤った libraries を注入しうるため、それに直接依存しない設計を明示している。CUDA はさらに強い host-bound の例である。`libcuda.so.1` はホストの NVIDIA kernel driver と結びついているため、それを Nix store に置くと、host driver の更新時に version mismatch を引き起こしうる。最近の [PyTorch/CUDA report](https://github.com/NixOS/nixpkgs/issues/461334) でも、binary package variant では動作するのに、runtime compilation では期待された runpath から `libnvrtc` を見つけられず失敗する例が示されている。これらは、runtime dynamic linkage と host-bound drivers が execution identity の一部であることを示している。

filesystem と policy の drift も実運用で観測される。`/bin/cp` の存在を前提とする Makefile は、`nix-shell` や Nix Docker image では動いても、`nix-build` では失敗しうる。なぜなら filesystem view が異なるからである。macOS では、完全な sandboxing が多くの build を壊すため、Nix builders が歴史的に弱い sandbox settings を用いてきた。ある [reported issue](https://github.com/NixOS/nix/issues/6049) では、Hydra の macOS builder が build 中に IPFS server を起動し、外部ネットワークトラフィックを発生させたことが報告され、より厳格な network sandbox 議論の契機となった。さらに、後の [Darwin regression](https://github.com/NixOS/nix/issues/11002) では、sandbox setup 中に多数の build-triggering commands が壊れた。これらの例は、sandbox strength、network policy、filesystem view が platform-specific な launch conditions であることを示している。

これらの観測は、Nix が弱いという証拠として読むべきではない。むしろ逆に、Nix は derivations、immutable store outputs、build-input explicitness に関して既存システムの中でも最も強いモデルの一つを提供している。大規模研究でも、nixpkgs における bitwise reproducibility は高く、改善し続けていると報告されている。ここでの論点はもっと狭い。Nix が主として識別するのは derivations と store outputs であり、source-native execution ではそれに加えて runtime、dependency projection、environment closure、dynamic library closure、filesystem view、network policy、capability policy、entrypoint、arguments、working directory、state bindings を識別する必要がある、という点である。

この意味で、Execution Identity は Nix を置き換えるのではなく補完する。

```text
Nix makes build inputs explicit.
Execution Identity makes launch conditions explicit.
```

### 2.4 Docker identifies images, not full launch envelopes

Docker は、移植可能な image と container execution model を提供する。Docker images は content-addressable digests を持ち、`docker run` は独自の filesystem、networking、process tree を持つ isolated process を生成できる。

しかし、Docker image identity は launch identity ではない。

A Docker run は単なる image digest ではなく、次の形をとる。

```text
docker run [OPTIONS] IMAGE[:TAG|@DIGEST] [COMMAND] [ARG...]
```

options は filesystem mounts、networking、environment variables、working directory、user、resource limits、capabilities などを設定しうる。Docker はまた、command、entrypoint、environment variables、user、working directory を含む image defaults が実行時に override 可能であることを文書化している。

したがって。

```text
Docker image digest identifies the image.
Execution Identity identifies the launch.
```

### 2.5 Package managers identify dependency resolutions

パッケージマネージャと lockfile は、エコシステム内での dependency choice を識別する。これらは不可欠だが、不完全である。通常、lockfile は次のものを識別しない。

```text
the runtime binary
the package manager binary
the platform ABI
the dynamic library closure
the environment closure
the filesystem view
the network policy
the entrypoint
the persistent state binding
```

さらに、lockfile が不変でも dependency output は変化しうる。原因として、lifecycle scripts、host-bound native builds、package manager version differences、registry behavior、platform-specific optional dependencies などがある。

したがって Execution Identity は次を分離する。

```text
dependency_derivation_hash = how dependencies were produced
dependency_output_hash     = what dependency tree was actually used
```

### 2.6 Source-native distribution needs launch-envelope identity

クロスプラットフォームな source distribution における問題は、次ではない。

```text
Can we ship source code?
```

本当の問題は次である。

```text
Can we distribute source code with enough identity to reconstruct and compare its execution launch conditions?
```

既存ツールが識別するのは隣接する対象である。

| System              | Identity target             | Strength                                    | Gap                                                      |
| ------------------- | --------------------------- | ------------------------------------------- | -------------------------------------------------------- |
| Reproducible Builds | artifact bytes              | bit-level artifact accountability           | does not identify launches                               |
| Bazel               | hermetic build actions      | host-independent build outputs              | build-centered                                           |
| Nix                 | derivations / store outputs | explicit build inputs and immutable outputs | not a general source-native launch identity              |
| Docker              | images                      | portable filesystem images and isolation    | image digest does not include full `docker run` envelope |
| Package managers    | dependency resolution       | ecosystem-native dependency graph           | runtime/env/fs/policy/entrypoint not included            |
| ReproZip            | traced execution bundle     | post-hoc capture of files/libs/env          | not pre-execution launch identity                        |

本論文は、Execution Identity をこの欠けていた identity object として提案する。

---

## 3. Execution Identity

私たちは **Execution Identity** を、source-native process launch envelope の content-addressed representation として定義する。

```text
execution_id = H(
  source_tree_hash,
  dependency_derivation_hash,
  dependency_output_hash,
  runtime_identity,
  environment_closure,
  filesystem_view_hash,
  network_policy_hash,
  capability_policy_hash,
  entry_point,
  argv,
  working_directory
)
```

execution identity は起動前に計算される。それが識別するのは、プロセスがこれから観測しようとしている world である。

### 3.1 Source identity

source identity は 2 つの部分からなる。

```text
source_ref       = where the source came from
source_tree_hash = what source tree was materialized
```

Git commit SHA は有用な provenance である。これは Git commit object を識別する。しかし、Git LFS resolution、checkout filters、line-ending normalization、generated files、symlink policy、platform-specific materialization の後に、ランタイムが実際に見る source tree と同一とは限らない。

Ato の hash policy は、Git commit SHA と、Ato が管理する source tree hash、payload hash、blob hash、derivation hash を明確に分離している。Git commit SHA は、Ato content integrity ではなく、source locator / provenance として扱われる。

この区別は、よくある category error を防ぐ。

```text
Git commit SHA is provenance.
source_tree_hash is materialized source identity.
```

### 3.2 Dependency identity

dependency identity は derivation と output に分かれる。

```text
dependency_derivation_hash = H(inputs and policies used to produce dependencies)
dependency_output_hash     = H(materialized dependency tree)
```

lockfile だけでは十分ではない。dependency output は次に依存しうる。

```text
package manager version
runtime version
platform
libc / ABI
package manager config
install command
lifecycle script policy
registry policy
network policy
environment allowlist
system build inputs
```

Ato の dependency derivation design は、この区別を明示している。`derivation_hash` は install input identity、`blob_hash` は frozen output identity である。また、lockfile digest 単体は dependency output の妥当な identity ではないとも述べている。

この区別は、クロスプラットフォーム実行において中心的である。同じ source と lockfile でも、Linux、macOS、Windows、glibc、musl、x86_64、arm64、GPU-enabled hosts 上では異なる dependency outputs を生みうる。

### 3.3 Runtime identity

runtime identity は、実際に用いられる executable runtime を識別する。

```text
runtime_identity = {
  declared: "node@20",
  resolved: "node@20.10.0",
  binary_hash: "sha256:...",
  abi: "linux-x64-glibc2.31",
  dynamic_linkage_fingerprint: "...",
  completeness: "binary-with-dynamic-closure"
}
```

declared version だけでは足りない。`node@20`、`python@3.11`、`ruby@3.3` は、system PATH、nvm、pyenv、asdf、mise、Volta、Homebrew、package managers、あるいは Ato-managed runtimes を通じて解決される可能性がある。version string が一致していても、binary や dynamic library closure は異なりうる。

したがって runtime identity には、declared identity と resolved identity の両方を含めるべきである。可能であれば dynamic linkage fingerprints も含めるべきである。難しい場合には、その identity がどこまで完全かを completeness level として記録すべきである。

```text
declared-only
resolved-binary
binary-with-dynamic-closure
best-effort
```

### 3.4 Environment closure

環境変数は execution inputs である。launch identity には、それらを明示的に含めなければならない。

```text
environment_closure = {
  env_vars: {
    "PATH": "<managed-runtime-bin>:<managed-tools-bin>",
    "HOME": "<session-home>",
    "LANG": "C.UTF-8",
    "TZ": "UTC"
  },
  fd_layout: {
    stdin: "inherited",
    stdout: "inherited",
    stderr: "inherited"
  },
  umask: "022",
  ulimits: { ... }
}
```

目標は、単にホスト環境を記録することではない。環境を閉じ、正規化することである。host variables は、明示的に含めるか、明示的に除外するかのどちらかであるべきだ。ある環境変数の変動を許すなら、その事実自体を execution identity に反映すべきである。

これは、Docker image identity が不十分である理由の一つでもある。Docker run-time options は、environment variables を含む image defaults を override しうる。

### 3.5 Filesystem view

プロセスは filesystem view を観測する。

```text
filesystem_view_hash = H({
  mounts: [
    { src: "store/blobs/<source>", dst: "/app", mode: "ro" },
    { src: "store/blobs/<deps>",   dst: "/app/node_modules", mode: "ro" },
    { src: "runs/<id>/tmp",        dst: "/tmp", mode: "rw" },
    { src: "state/<app>/data",     dst: "/data", mode: "rw" }
  ],
  case_sensitivity: "...",
  symlink_policy: "...",
  tmp_policy: "session-local"
})
```

この view には、read-only な source layers、dependency projections、書き込み可能な session caches、一時ファイルシステム、persistent state bindings が含まれうる。view の identity は、そのどれか 1 層の identity と同値ではない。

Docker image digest が識別するのは image である。bind mounts、volumes、tmpfs、state attachments、working directory overrides までは識別しない。

### 3.6 Policy identity

policy は execution に影響する。

```text
policy_identity = H({
  network: {
    mode: "deny-by-default",
    allow: ["api.example.com"]
  },
  capabilities: {
    fs_read: [...],
    fs_write: [...],
    host_bridge: [...]
  },
  sandbox: {
    backend: "landlock+bwrap",
    strength: "strict"
  }
})
```

ネットワークアクセスが許可されたプロセスと、そうでないプロセスは、等価な launch conditions を持たない。ホスト secrets の読み取りが許されたプロセスと、拒否されたプロセスも、等価な launch conditions を持たない。

したがって policy は execution identity の一部である。

---

## 4. Reproducibility Classes

クロスプラットフォームな execution reproducibility は二値ではない。正しい問いは、その execution が再現可能かどうかだけではなく、**なぜ** 再現可能なのか、あるいは **なぜ** 再現可能でないのかである。

私たちは 6 つの reproducibility classes を定義する。

```text
reproducibility_class ∈ {
  pure,
  host-bound,
  state-bound,
  time-bound,
  network-bound,
  best-effort
}
```

### 4.1 Pure

`pure` execution とは、execution identity だけから再生できると期待される execution である。

例:

```text
sealed source tree
sealed dependency output
sealed runtime binary
closed environment
read-only filesystem view
no network
no persistent state
fixed entrypoint and argv
```

### 4.2 Host-bound

`host-bound` execution は、host ABI、kernel、driver、CPU feature、GPU runtime、libc、system libraries、あるいは他の非移植的な host properties に依存する。

例:

```text
native Python extension linked against host libraries
node-gyp module compiled against host libc
GPU workload depending on driver version
```

### 4.3 State-bound

`state-bound` execution は、persistent state あるいは過去の state に依存する。

例:

```text
application database
browser profile
model cache
user workspace
previous generated files
```

state snapshot が含まれるか参照されるなら、state-bound execution も replay 可能でありうる。

### 4.4 Time-bound

`time-bound` execution は、wall-clock time、monotonic time、timezone、scheduled behavior に依存する。

例:

```text
date-sensitive tests
license checks
scheduled jobs
```

### 4.5 Network-bound

`network-bound` execution は、外部ネットワーク応答に依存する。

例:

```text
live API call
registry lookup
model download
remote feature flag
```

### 4.6 Best-effort

`best-effort` execution は、制御されていない、あるいは分類されていない非決定性を含む。

例:

```text
opaque installer
unclassified lifecycle script
host tool invocation outside sandbox
untracked dynamic dependency
```

本論文の貢献は、すべての execution が再現可能だと主張することではない。貢献は、非再現性の原因と程度を識別可能にすることにある。

```text
We do not claim that all executions are reproducible.
We make non-reproducibility identifiable.
```

---

## 5. Ato: A Reference Runtime for Execution Identity

Ato は、ローカルプロジェクト、GitHub リポジトリ、共有アプリケーションハンドルのための source-native execution runtime である。その README は、プロジェクトが必要とするものを検出し、不足しているツールを準備し、Python、Node、Rust あるいはプロジェクト固有の依存物をユーザーが手動インストールしなくても実行できる command-line tool として Ato を説明している。

Ato は、Nix、Docker、パッケージマネージャの代替として提示されるものではない。Ato は、source-native launches のための Execution Identity の reference implementation として位置づけられる。

### 5.1 Layered state

Ato は、session effects、persistent user state、immutable materialized objects を分離する。

```text
~/.ato/
├── runs/
│   └── <session>/
│       ├── workspace/source/
│       ├── workspace/build/
│       ├── deps/
│       ├── cache/
│       └── tmp/
│
├── state/
│   └── <capsule-id>/
│       └── data/
│
└── store/
    ├── blobs/<blob-hash>/
    ├── refs/
    ├── meta/
    └── attestations/
```

Ato の dependency materialization design から得られる core invariant は次である。

```text
store/blobs/<blob-hash>/       immutable payload only
runs/<session>/deps/           dependency projection
runs/<session>/cache/          writable session cache
state/<capsule-id>/data/       writable persistent user state
```

この分離は launch identity にとって必要である。dependencies、source tree、persistent state、session cache が混ざってしまうと、execution drift は説明できない。

### 5.2 Source-tree non-pollution

Ato の Phase A0 は source-tree non-pollution である。dependencies や build outputs は、ユーザーの project directory や install 済み capsule directory に書き込まれるべきではない。すべての session-local side effects は `runs/<id>/` 配下に閉じ込められる。

この phase は、意図的に full CAS optimization ではない。これは correctness phase である。ある execution を再現できるようにする前に、どの effect が source に属し、どれが run に属するのかをシステムが知っていなければならない。

### 5.3 Dependency materialization

Ato は dependency installation を単一の `DependencyMaterializer` にルーティングする。

概念的には次の通りである。

```text
request
  -> create session workspace
  -> compute derivation identity
  -> lookup dependency output blob
  -> install on miss
  -> freeze output
  -> project into run session
```

この結果、dependency derivation identity と dependency output identity の両方が得られる。

Ato は、file-level CAS ではなく whole-tree dependency output caching から始める。その設計は、file-level CAS をデフォルトとして採用しない。理由は、複雑であり、stack traces や source maps を読みにくくし、debugging workflows を悪化させ、filesystem と inode pressure を増やすからである。

ここに、慎重な Nix 比較を置くことができる。

```text
Nix-style store discipline is powerful for package universes.
Ato-style materialization is optimized for exploratory source-native launches.
```

したがって Ato は、dependency output identity の構成要素として Nix store paths や derivation metadata を利用できる。一方で、その identity boundary は launch envelope まで外側に拡張される。すなわち、runtime selection、dependency projection、environment closure、filesystem view、network policy、capability policy、entrypoint、arguments、working directory、state bindings である。

### 5.4 Hash-domain separation

Ato は hash domains を分離する。

```text
Git commit SHA      = source locator / provenance
source_tree_hash    = materialized source identity
derivation_hash     = dependency input identity
payload_hash        = artifact integrity
blob_hash           = immutable store object identity
```

hash policy は、これらの domains を 1 つの hash に折りたたむべきではないと明示している。

Execution Identity は、この原則を full launch envelope へと一般化する。

### 5.5 Run and install

Ato は `run` と `install` を区別する。

```text
ato run     = ephemeral session + reusable verified materialization
ato install = persistent app identity + state + permissions + refs
```

この区別が重要なのは、install が persistent identity と state binding を導入するからである。run は `pure` にも `best-effort` にもなりうるが、install はしばしば `state-bound` になる。

---

## 6. Replay and Drift

### 6.1 Replay as launch-envelope reconstruction

Execution Identity は replay を可能にするが、replay は慎重に定義しなければならない。

```text
replay(execution_id) = reconstruct the pre-execution launch envelope
```

これは deterministic trace replay ではない。identical instruction traces、syscall ordering、timing、scheduler behavior、network responses を保証するものではない。

ReproZip は、command 実行後に OS calls を用いて trace を取り、将来の再実行に必要な binaries、files、libraries、dependencies、environment variables を識別する。Execution Identity との違いは、Execution Identity が launch envelope を実行前に識別することにある。

replay command は次のようになるかもしれない。

```text
ato replay <execution_id>
```

それは次を行う。

1. execution receipt を解決する。
2. source と dependency blobs を取得または検証する。
3. runtime identity を解決する。
4. environment closure を再構成する。
5. filesystem view を再構成する。
6. network policy と capability policy を適用する。
7. 同じ entrypoint、arguments、working directory で起動する。

### 6.2 Drift

drift とは、見かけ上は似た user intent から、異なる execution identities が生成されることである。

```text
drift = same source_ref, different execution_id
```

例:

```text
same Git branch, different commit
same commit, different LFS materialization
same source tree, different dependency output
same lockfile, different package-manager version
same runtime version, different binary hash
same dependency tree, different environment closure
same source and deps, different filesystem mount
same app, different persistent state binding
```

これにより、「works on my machine」は identity problem として再定式化される。run が神秘的に変化したのではない。launch envelope が変わったのである。

---

## 7. Evaluation

### 7.1 Execution identity stability

同じ source project を、制御された条件と摂動を与えた条件の両方で繰り返し実行する。

条件:

```text
same host, same day
same host, different day
same OS, different machine
different OS
different runtime manager
different environment variables
different timezone
different mount layout
different state binding
```

指標:

```text
execution_id stability
component-level diff
classification assigned
false stability rate
false drift rate
```

### 7.2 Drift detection

launch component を 1 つずつ意図的に変更する。

| Perturbation                          | Expected changed component         |
| ------------------------------------- | ---------------------------------- |
| Replace Node binary                   | `runtime_identity`                 |
| Change `PATH`                         | `environment_closure`              |
| Add bind mount                        | `filesystem_view_hash`             |
| Change timezone                       | `environment_closure`              |
| Same lockfile, different pnpm version | `dependency_derivation_hash`       |
| Lifecycle script downloads binary     | `dependency_output_hash` and class |
| Add persistent database binding       | `filesystem_view_hash` and class   |

### 7.3 Replay success by class

launch-envelope reconstruction として replay を測定する。

| Class           | Expected replay behavior                     |
| --------------- | -------------------------------------------- |
| `pure`          | high launch and behavior consistency         |
| `host-bound`    | high on same host, lower cross-host          |
| `state-bound`   | high with state snapshot                     |
| `time-bound`    | high if clock is pinned                      |
| `network-bound` | launch replay possible, behavior may diverge |
| `best-effort`   | diagnostic only                              |

### 7.4 Source-tree non-pollution

Node、Python、Rust、mixed-language projects からなるコーパスに対して、execution 前後の source tree を比較する。

指標:

```text
new files in source tree
modified files in source tree
deleted files in source tree
untracked dependency directories
untracked build outputs
```

期待結果: Ato の A0 が dependency と build-output pollution を防ぐ。

### 7.5 Cross-platform execution drift

同じ source reference を Linux、macOS、Windows で実行する。

測定項目:

```text
which components differ
whether differences are expected
whether differences are explained by reproducibility class
whether an equivalent launch envelope can be reconstructed
```

この評価は、本論文の中心問題であるクロスプラットフォームな source-native distribution を直接扱う。

---

## 8. Related Work

### 8.1 Reproducible Builds

Reproducible Builds は、同じ source、build environment、build instructions から artifact をビット単位で一致させることを artifact reproducibility と定義する。Execution Identity が扱うのは artifact reproducibility ではなく launch reproducibility である。

### 8.2 Bazel and hermetic builds

Bazel の hermeticity は、build を host changes から隔離し、特定バージョンの tools と dependencies を用いることで安定した outputs を生む。Execution Identity は、同様の explictness を runtime launch conditions に適用する。

### 8.3 Nix

Nix derivations は、system type、builder、arguments、environment variables、outputs を含む explicit inputs を持つ build tasks を記述する。これは最も近い哲学的基盤だが、identity object が異なる。Nix が識別するのは derivations と store outputs であり、Execution Identity が識別するのは source-native launches である。

### 8.4 Docker and OCI

Docker は、image-based execution を移植可能にする。しかし、`docker run` は image identity に加えて、options、command、arguments、environment variables、mounts、working directory、user、networking を組み合わせる。Execution Identity は、この complete launch envelope を識別する。

### 8.5 ReproZip

ReproZip は、実行済み command を trace し、将来の再実行のために binaries、files、libraries、dependencies、environment variables をパッケージ化する。これは post-hoc capture である。Execution Identity は pre-execution identity である。

### 8.6 Package managers and lockfiles

パッケージマネージャは dependency graph と resolution を識別する。これは必要だが、runtime、environment、filesystem view、policy、entrypoint、state までは識別しない。

---

## 9. Discussion

### 9.1 “Same execution result” is too strong

本論文は、同じ source code がすべてのプラットフォームで同じ behavior を生むとは主張しない。その主張は強すぎる。異なる kernels、filesystems、clocks、drivers、GPU stacks、external services、random sources は behavior に影響しうる。

本論文の主張はより狭い。

```text
The same execution_id identifies equivalent launch conditions.
```

その上で、どの程度の behavioral reproducibility を期待できるかは reproducibility classes が説明する。

### 9.2 Why not require Nix?

Nix は強力だが、あらゆる source-native project に対して Nix package 化を要求すると、ユーザーのワークフローは変わってしまう。多くのユーザーやエージェントは、package 化する前に repository を走らせたい。Execution Identity はこのより手前の phase を扱う。

Ato は、Nix の full package-universe model を採用せずに、Nix から借りられるものを借りられる。

```text
Borrow explicit input identity.
Borrow immutable store objects.
Borrow closure thinking.
Do not require source-native execution to become package authoring.
```

### 9.3 Why not use Docker images?

Docker images は優れた distribution artifacts である。しかし launch は image そのものではない。launch は image に加えて、options、mounts、environment、command、arguments、policy、state からなる。Execution Identity は launch を対象にする。

### 9.4 Why not deterministic record/replay?

record/replay systems は execution traces を捕捉する。これは launch identity より強いが、より狭く、より重い。deterministic replay が不可能な場合でも Execution Identity は有用である。なぜなら、実行が始まる前に drift を説明できるからである。

### 9.5 Security and privacy

execution receipts には機密情報が含まれうる。paths、environment variable names、state bindings、policy rules、dependency information などである。secret values 自体を直接記録してはならない。receipts は sensitive fields を分類し、redact できなければならない。

---

## 10. Conclusion

クロスプラットフォームな source-native software distribution には、再現可能ビルド、package locks、container image digests、事後的 execution traces だけでは足りない。

Reproducible Builds は artifacts を識別する。Nix は derivations と store outputs を識別する。Docker は images を識別する。パッケージマネージャは dependency resolutions を識別する。ReproZip は、実行後に観測された execution dependencies を捕捉する。

Execution Identity が識別するのは launches である。

Execution Identity は、source tree identity、dependency derivation and output identity、runtime identity、environment closure、filesystem view、network and capability policy、entrypoint、arguments、working directory をハッシュ化することによって、source-native execution を説明可能にする。reproducibility classes は、その launch が pure、host-bound、state-bound、time-bound、network-bound、best-effort のどれに当たるかを説明する。

Ato は、このモデルを source-native runtime として実装する。Ato は session-local effects、persistent user state、immutable store objects、mutable refs、execution metadata を分離する。その目標は Nix、Docker、パッケージマネージャを置き換えることではなく、クロスプラットフォームな source execution に不足していた identity layer を定義することである。

中心的な主張は次である。

```text
Nix makes build inputs explicit.
Docker makes filesystem images portable.
Execution Identity makes launches accountable.
```

---

# Appendix A: Revised one-paragraph pitch

再現可能ビルドは artifacts を説明可能にしたが、クロスプラットフォームな source distribution には launches を説明可能にすることが必要である。同じ source tree でも、runtime resolution、dependency materialization、dynamic libraries、environment variables、filesystem mounts、network policy、persistent state、locale、timezone、entrypoint、arguments、working directory によって異なる実行になりうる。既存システムが識別するのは隣接する対象である。Nix は derivations と store outputs を、Docker は images を、パッケージマネージャは dependency resolutions を識別し、ReproZip は実行後に traces を捕捉する。私たちは、pre-execution launch envelope の content-addressed representation として Execution Identity を提案する。execution identity によって、すべてのプログラムに対して不可能な決定的挙動を主張することなく、クロスプラットフォームな source-native executions の比較、replay、drift の説明が可能になる。

---

# Appendix B: 強い一文候補

```text
Reproducible builds answer: did we produce the same artifact?
Execution Identity answers: did we launch the same world?
```

```text
Nix makes build inputs explicit.
Docker makes filesystem images portable.
Execution Identity makes launches accountable.
```

```text
The launch, not the image, is the unit of execution reproducibility.
```

```text
A Git commit identifies where the source came from.
A lockfile identifies dependency intent.
An image digest identifies a filesystem artifact.
An execution identity identifies the world a process was launched into.
```

```text
Works on my machine is not a mystery; it is an unaccounted launch envelope.
```