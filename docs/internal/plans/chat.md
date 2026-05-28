> From: https://chatgpt.com/g/g-p-69f5606d10a4819186df6182fe7cab13-ato/c/6a0ffbb3-5958-83a5-9c29-1d6f3d5fa467

# you asked

AFFiNEをAtoで動かせる？

---

# chatgpt response

結論: **動かせる可能性は高いですが、現時点で `ato run github.com/toeverything/AFFiNE` 一発で source-native に動く対象ではありません。**  
最短ルートは **OCI / Docker Compose 相当の capsule 化**、本命ルートはその後の **source-native 化** です。

AFFiNE は単純な Node アプリではなく、Node + Yarn 4 + Rust native module を持つ大きめの monorepo です。公式の build docs でも Node.js と Rust toolchain が必要で、native module と server-native module の build 手順が明記されています。現在の `.nvmrc` は Node `22.22.3`、`.yarnrc.yml` は Yarn `4.13.0` を指しています。([GitHub](https://raw.githubusercontent.com/toeverything/AFFiNE/master/docs/BUILDING.md))([GitHub](https://raw.githubusercontent.com/toeverything/AFFiNE/master/.nvmrc))([GitHub](https://raw.githubusercontent.com/toeverything/AFFiNE/master/.yarnrc.yml))
さらに self-host 側は Docker 前提が強いです。GitHub README でも self-host は Docker から始める案内になっており、実際の compose 例では AFFiNE server、migration job、Redis、Postgres、永続 volume、`DATABASE_URL` / `REDIS_SERVER_HOST` が必要です。([GitHub](https://github.com/toeverything/AFFiNE))([GitHub](https://github.com/toeverything/AFFiNE/issues/12155))非 Docker install については、maintainer が「現在 AFFiNE server は non-docker installation を考慮しておらず、edge case と dependency issue が多い」と述べています。([GitHub](https://github.com/toeverything/AFFiNE/discussions/13596))
Ato 側は GitHub repo を直接実行し、必要な tool/runtime を検出して controlled runtime で動かす設計ですが、AFFiNE は推論だけに任せるには複雑すぎます。 また、Ato 0.5.1 時点では少なくとも複数 toolchain + build lifecycle の接続に不足があり、`runtime_tools` と `build` hook を正しく通す必要がある、という既知の課題があります。

実装方針としてはこうです。

まず **Phase 1: OCI capsule**。AFFiNE の公式 self-host 構成を Ato の service graph に落とす。`affine`、`affine_migration`、`redis`、`postgres` を services として定義し、`depends_on` と readiness を張る。port は 3010、state は `storage`、`config`、`postgres/pgdata` を persistent にする。この方式なら AFFiNE 側の想定に最も近く、Ato の OCI / service orchestration の検証にも使いやすいです。

イメージはこうです。これは仕様イメージで、現在の parser に合わせて調整が必要です。

```toml
schema_version = "0.3"
name = "affine"
version = "0.26.3"
type = "app"
default_target = "affine"

[targets.affine]
runtime = "oci"
image = "ghcr.io/toeverything/affine:stable"
port = 3010

[targets.migration]
runtime = "oci"
image = "ghcr.io/toeverything/affine:stable"
run = "node ./scripts/self-host-predeploy.js"

[targets.redis]
runtime = "oci"
image = "redis:latest"

[targets.postgres]
runtime = "oci"
image = "postgres:16"

[services.postgres]
target = "postgres"

[services.redis]
target = "redis"

[services.migration]
target = "migration"
depends_on = ["postgres", "redis"]

[services.main]
target = "affine"
depends_on = ["postgres", "redis", "migration"]

[state.affine_storage]
kind = "filesystem"
durability = "persistent"
purpose = "AFFiNE uploaded files and blobs"

[state.affine_config]
kind = "filesystem"
durability = "persistent"
purpose = "AFFiNE config"

[state.affine_postgres]
kind = "filesystem"
durability = "persistent"
purpose = "Postgres data"
```

次に **Phase 2: source-native capsule**。これは Ato らしさは強いですが、難度が高いです。Node 22.22.3、Yarn 4.13.0、Rust toolchain、native module build、server-native build、Postgres / Redis provider、migration lifecycle、永続 state、readiness probe を全部 capsule.toml に明示する必要があります。AFFiNE 公式が non-Docker install を主導していないので、ここは Ato 側でかなり吸収する形になります。

判断としては、**今すぐユーザーに見せるなら OCI 版から**。  
**Ato の技術的デモにするなら source-native 版を別ブランチで挑戦**が妥当です。AFFiNE は「Atoで大規模 self-host app を動かせるか」の良いベンチマークですが、初手の acceptance target にするには重すぎます。Blinko や WasedaP2P の次の段階、つまり OCI/service graph の実力検証として扱うのがよいです。

---

# you asked

ビルドサーバーを持って、Atoでシステム側の依存関係(execution identity)をserverに送り、そこむけに適切な依存関係でビルドして渡すのは？

---

# chatgpt response

かなり良いです。Atoの方向性としては **Remote Build Server** というより、**Remote Materialization / Binary Substitute for Execution Identity** と呼ぶ方が正確です。

ただし、**execution identity を丸ごと server に送る**設計にはしない方がいいです。Execution Identity には runtime identity、env closure、filesystem view、network policy、entrypoint、cwd まで含まれるので、launch 全体の識別子です。Ato の論文整理でも、Execution Identity は「成果物」ではなく「起動条件」を識別するものとして定義されています。  
server に送るべきなのは、そのうち **build / dependency materialization に必要な安全な部分集合** です。

設計としてはこうです。

```text
client
  ↓
compute materialization request
  - source_tree_hash
  - capsule.toml / ato.lock.json digest
  - target
  - build spec
  - dependency derivation hash
  - host capability profile
      os = darwin/linux/windows
      arch = arm64/x86_64
      libc = glibc/musl/none
      cpu baseline
      gpu/cuda if needed
      sandbox/backend constraints
  - build-time env allowlist digest only
  ↓
Ato build server
  ↓
returns signed materialized outputs
  - runtime tool refs
  - dependency output blob
  - build output blob
  - SBOM
  - provenance / logs
  - dependency_output_hash
  - build_output_hash
  ↓
client verifies hash + signature
  ↓
local execute with secrets/state/network policy
```

ここで重要なのは、server が返すのは **Docker image** ではなく、Ato の execution graph に差し込める **materialized dependency/output layer** です。Ato 側にはすでに build spec・source・lockfile・toolchain fingerprint から Build Materialization の input digest を作る考え方があるので、この延長に置けます。

位置づけは Nix の binary cache / substitute に近いです。ただし違いは、Nix が主に derivation / store output を配るのに対して、Ato は **source-native launch envelope の一部を remote materialize する**ことです。つまり「ビルド結果をもらう」のではなく、「この起動条件に必要な依存層を、対象プラットフォーム向けに事前生成してもらう」。ここがAtoらしいです。

AFFiNEのようなケースではかなり効きます。ローカルで Node 22、Yarn 4、Rust native module、巨大な monorepo build を毎回抱えるのではなく、server が `darwin-arm64` / `linux-x86_64-glibc` 向けに native modules と build output を作り、client は検証済み blob を受け取って実行だけする。DB/Redis/永続 state/secrets はローカルの実行面に残す。これなら「重いビルドはサーバー、実行はローカル」というAtoの価値に合います。

ただし分離は必須です。

```text
remote に送ってよい:
  source hash
  public repo ref
  lock digest
  target platform profile
  build spec
  dependency derivation inputs

remote に送らない:
  secrets
  .env 実値
  user HOME path
  local absolute paths
  persistent user state
  runtime session id
  private files unless explicit consent
```

実装順はこれが安全です。

1. **Local Build Materialization key を厳密化**
   `execution_id` とは別に、`remote_materialization_key` を導入する。これは launch 全体ではなく、remote build 可能な部分だけの digest にする。

2. **Public binary substitute cache**
   まずは Ato公式がビルド済み依存層を公開するだけにする。ユーザーソースのアップロードはしない。JSON Hero、Blinko、AFFiNE のような代表例で効果を出す。

3. **On-demand public GitHub build**
   `github.com/owner/repo@commit` に対して server が worker pool で materialize。結果を CAS に保存し、同じ key なら全員が再利用。

4. **Private build**
   private repo / local source upload は後回し。暗号化、ACL、削除、課金、監査ログが必要になる。

5. **Cross-platform worker pool**
   `linux-x86_64-glibc`, `linux-arm64`, `darwin-arm64`, `windows-x86_64` の worker を分ける。特に macOS はライセンス・署名・native module の扱いが面倒なので後段でよいです。

注意点は、これを「Ato cloud build」と呼ぶと普通のCI/CDに見えます。訴求上は **“Ato pre-materializes the launch envelope”** の方が強いです。  
技術的にも、これは単なるビルドサーバーではなく、AtoのExecution Identityを実用化するための **remote materialization backend** です。

結論として、採用すべきです。ただし最初からフル execution identity を送るのではなく、**Execution Identity → RemoteMaterializationRequest へ射影する**設計にしてください。これなら速さ、再現性、プライバシー、Atoの独自性が両立します。

---

# you asked

localのマシーン上で開ける独自フォーマットにするのはどう？

---

# chatgpt response

かなり良いです。Atoには **「URLで実行」だけでなく「ローカルで開ける実行可能レシピファイル」** があった方が強いです。

ただし、新しい独自形式をゼロから作るより、既存の構想にある **`.capsule` を正本にする**のがよいです。仕様上も `.capsule` は `capsule.toml`、`capsule.lock.json`、SBOM、署名、payload を持つアーティファクト形式として整理されています。

位置づけはこうです。

```text
.capsule
  = Ato Desktop / ato CLI で開ける
  = source + launch recipe + lock + provenance + optional materialized layers
  = ローカルで検証してから実行するファイル
```

これは `.app`、`.dmg`、`.exe`、Docker image の代替ではなく、**Ato が解釈する launch bundle** です。Atoの強みは「成果物」ではなく「起動条件」を識別する Execution Identity にあるので、ファイルも単なるビルド済みバイナリではなく、起動条件・依存層・検証情報を持つべきです。Execution Identity は source tree、dependency derivation/output、runtime、environment、filesystem view、network policy、entrypoint、argv、cwd などを含む launch envelope として定義されています。

おすすめは二層構造です。

```text
Public format:
  .capsule
    人間が配る・ダブルクリックで開く・共有する形式

Internal/cache format:
  .ato-layer / CAS blob
    依存関係やビルド成果物を content-addressed に保存する内部形式
```

`.capsule` に全部を詰めてもいいですが、依存 layer は重くなります。なので `.capsule` は基本的に manifest / lock / source / provenance を持ち、必要なら platform-specific な materialized layer を同梱する。入っていなければ Ato が build server / cache から取得する、という設計がきれいです。

ユーザー体験はこうです。

```text
1. someone sends affine.capsule
2. user double-clicks
3. Ato Desktop opens verification screen
4. shows:
   - publisher
   - source provenance
   - target platform
   - required services
   - network permissions
   - required secrets
   - state directories
5. user clicks Run
6. Ato imports blobs into local store
7. missing secrets/state only localで補完
8. launch
```

中身の案はこうです。

```text
affine.capsule
├── capsule.toml
├── capsule.lock.json
├── execution.identity.json
├── materialization.index.json
├── sbom.spdx.json
├── provenance.json
├── signature.json
├── README.md
└── payload.tar.zst
```

`materialization.index.json` には、例えばこういう情報を置きます。

```json
{
  "platform": "linux-x86_64-glibc",
  "remote_materialization_key": "blake3:...",
  "layers": [
    {
      "kind": "dependency_output",
      "hash": "blake3:...",
      "path": "blobs/deps.tar.zst"
    },
    {
      "kind": "build_output",
      "hash": "blake3:...",
      "path": "blobs/build.tar.zst"
    }
  ]
}
```

重要なのは、**ユーザーの secrets / `.env` 実値 / HOME の絶対パス / persistent state は絶対に入れない**ことです。`.capsule` は「実行可能な配布物」ではあるが、「ユーザー環境のコピー」ではない。state は `durability = "persistent"` の要求だけを記録し、実体はローカル ATO_HOME 側で作るべきです。

また、`.capsule` は必ず streaming verification 可能にした方がいいです。既存仕様でも `capsule.toml` と `signature.json` を先に読み、manifest hash、署名、payload hash を検証してから展開する流れになっています。展開時も絶対パス、`../` traversal、危険なリンク、デバイスファイルを拒否する設計が入っています。

結論としては、**賛成**です。ただし名前は新規の `.atoapp` より、まず **`.capsule` をローカルで開ける正式フォーマットにする**のがよいです。  
その上で、platform-specific な事前ビルド入りの場合だけ `materialized capsule` と呼ぶ。これなら「ソースをレシピとして配る」と「重い依存関係は事前生成して高速起動する」が両立します。

---

# you asked

この場合、プラットフォームの署名の強制やデベロッパープログラムの必須などの制約を回避できる？

---

# chatgpt response

結論: **「回避」ではなく「制約の適用対象をAto本体に集約する」ことはできます。**

`.capsule` を **実行ファイルではなくデータファイル** として扱うなら、各 capsule 作者が Apple Developer Program や Windows のコード署名証明書を必ず持つ必要はかなり下げられます。ユーザーは署名済み・notarize済みの **Ato Desktop / Ato CLI** をインストールし、`.capsule` は Ato が検証して実行する、という構造です。

ただし、OSのセキュリティを無視して未署名コードを強制実行できるわけではありません。macOS では、Mac App Store外で配布するアプリは Gatekeeper が Developer ID を確認し、notarization も実務上必要です。Apple は、外部配布アプリ・プラグイン・installer package は署名し、notarize することを案内しています。([Apple Developer](https://developer.apple.com/developer-id/?utm_source=chatgpt.com))また、quarantine されたアプリを起動すると Gatekeeper が介入します。([Apple Developer](https://developer.apple.com/forums/tags/gatekeeper?page=2&utm_source=chatgpt.com))
したがって設計上の整理はこうです。

```text
署名・notarization が必要:
  Ato Desktop
  Ato CLI
  Ato helper / sidecar / updater
  Ato が配布する native runtime shim

各開発者に必須ではなくできる:
  capsule.toml
  source bundle
  lockfile
  dependency layer
  build output layer
  .capsule fileそのもの

注意が必要:
  capsule内に含めた .app / .dylib / .so / .exe / native helper
  system extension / driver / kernel extension
  privileged helper
  browser extension
```

Windows でも同じです。Ato本体はコード署名して SmartScreen reputation を積む必要があります。Microsoft は SmartScreen が downloaded program と digital signature の reputation を評価すると説明しており、Microsoft Store 経由なら再署名され reputation warning が出にくくなります。([Microsoft Learn](https://learn.microsoft.com/en-us/windows/security/operating-system-security/virus-and-threat-protection/microsoft-defender-smartscreen/?utm_source=chatgpt.com))一方で `.capsule` を単なるデータとして扱うなら、個々の capsule 作者が毎回 `.exe` を署名して配る構造よりは摩擦を下げられます。

Atoとしての理想形は、三層署名です。

```text
1. Platform signature
   Apple / Microsoft / Linux package signing
   対象: Ato本体

2. Capsule signature
   Ed25519 / did:key / Ato registry identity
   対象: capsule作者・source・manifest・lock

3. Materialization signature
   Ato build server signature / provenance / SBOM
   対象: 依存layer・build output layer
```

この構造にすると、AppleやMicrosoftには「Atoという実行基盤」を信頼してもらい、Ato内部では「誰がどの capsule を発行したか」「どの build server が materialize したか」を独自に検証できます。つまり、各アプリ作者にOSベンダーの開発者アカウントを要求する代わりに、**Ato registry / capsule signature / execution consent** で信頼を表現する形です。

ただし、ここは誤解しない方がいいです。これは **platform policy bypass** ではありません。Ato が未署名バイナリを勝手に起動したり、quarantine を外したり、Gatekeeper を迂回したりする設計にすると、OSやAVから不審なランチャー扱いされます。やるべきなのは、Ato本体を正規に署名し、`.capsule` を透明に検証し、native executable を含む場合は追加警告・署名検査・sandbox制限をかけることです。

プロダクト上は、かなり強い価値があります。

```text
従来:
  小さなOSS作者も macOS/Windows 向け配布の署名・notarization・installer を考える必要がある

Ato:
  作者は .capsule / source recipe を発行する
  Ato本体が platform trust を引き受ける
  capsule trust は Ato の署名・provenance・consent UI で扱う
```

要するに、**各アプリごとのDeveloper Program必須化はかなり緩和できる**。  
ただし、**Ato本体は正面から署名・notarization・SmartScreen reputation を取りに行く必要がある**、という判断です。

---

# you asked

作者というより、それを動かしたい人がカプセルのレシピを考えるようなイメージです。

---

# chatgpt response

そのイメージの方がAtoには合っています。

つまり `.capsule` は「作者が公式に配るアプリパッケージ」ではなく、**“このソフトを自分の環境で動かすための実行レシピ”** です。近い比喩は Homebrew formula、Nix expression、Docker Compose、Arch PKGBUILD ですが、Atoの場合はそれを **ローカル実行・Execution Identity・Desktop UX** に寄せる。

役割分けはこうするのがよいです。

```text
Upstream author
  ソースコードを書く人
  例: AFFiNE 開発者

Recipe author / runner / curator
  そのソースをAtoで動かす方法を書く人
  例: AFFiNEをAtoで動かしたいユーザー、コミュニティ、Ato公式

Ato build server
  レシピに基づいて依存層・ビルド成果物を materialize する人

Ato runtime
  ローカルで検証・隔離・起動する実行基盤
```

この構造なら、プラットフォーム署名の話もかなり整理できます。OSから見ると、実行基盤は署名済みのAto本体。`.capsule` は「第三者が書いた実行レシピ」であって、macOSの `.app` や Windows の `.exe` ではない。したがって、各ソフト作者や各レシピ作者に Apple Developer Program / Windows code signing を要求する必要は薄くできます。

ただし、信頼表示では「作者」を1つに潰さない方がいいです。

```text
Source:
  github.com/toeverything/AFFiNE @ commit ...

Recipe:
  affine.ato.recipe by kohsuke / ato-community

Materialized by:
  Ato Build Server linux-x86_64-glibc

Verified:
  source hash, lock hash, dependency output hash, build output hash

Permissions:
  network, state, services, secrets
```

ここで重要なのは、**upstream author と recipe author を明確に分離すること**です。AFFiNEの開発者がAto対応していなくても、誰かが「AFFiNEをAtoで動かすレシピ」を作れる。これはAtoの強みになります。

フォーマット名も、`.capsule` の中に「アプリ本体」ではなく「レシピ＋ロック＋検証情報＋必要ならmaterialized layer」が入る、という理解がよいです。

```text
affine.capsule
├── capsule.toml              # 実行レシピ
├── ato.lock.json             # 解決済み依存・実行条件
├── source.provenance.json    # upstream repo / commit / source hash
├── materialization.json      # 事前ビルド依存層の参照
├── policy.json               # network / env / state / secrets
├── sbom.spdx.json
└── signature.json            # recipe author / build server の署名
```

プロダクトとしては、Ato Storeも「公式アプリストア」ではなく、最初は **Recipe Registry** として考える方が自然です。

```text
Search: AFFiNE
  → Upstream repo found
  → Community recipe exists
  → Verified on darwin-arm64 / linux-x86_64
  → Run
```

ユーザーが動かせたら、そのレシピを投稿できる。

```text
ato run github.com/toeverything/AFFiNE
ato recipe init
ato recipe test
ato recipe publish
```

この流れはかなり強いです。ソフト作者を巻き込まなくても、ユーザー側・コミュニティ側から「動くレシピ」が増える。Atoはそれを検証し、Execution Identity と materialization cache で再利用可能にする。

結論として、Atoの中心概念は「作者が配るアプリ」より **“runner-authored executable recipe”** に寄せた方がいいです。  
その方が、GitHub上の既存OSSをAtoエコシステムに取り込む速度が上がります。

---

# you asked

実行基盤はどういう風にする ？

---

# chatgpt response

実行基盤は、**Ato本体を署名済みの“レシピ実行インタプリタ”にして、個々の capsule は不信なデータとして扱う**構成がよいです。  
つまり、`.capsule` はアプリではなく **Ato Runtime に渡す宣言ファイル**。実行権限・隔離・署名検証・依存取得・プロセス管理は全部 Ato 側に集約します。

構成はこうです。

```text
Ato Desktop / CLI
  ├─ Recipe Resolver
  ├─ Policy & Consent Broker
  ├─ Materialization Engine
  ├─ Local CAS Store
  ├─ Runtime Adapter
  │    ├─ source runtime
  │    ├─ OCI runtime
  │    ├─ WASM runtime
  │    └─ web/static runtime
  ├─ Session Manager
  └─ Sandbox / Network / State Controller
```

一番重要なのは、**recipe layer と runtime layer を分離する**ことです。

```text
recipe layer:
  capsule.toml
  ato.lock.json
  source provenance
  service graph
  required secrets
  network policy
  state declaration

runtime layer:
  source checkout
  dependency layer
  build output layer
  process spawn
  sandbox
  local ports
  lifecycle
```

Atoの既存思想にも合っています。Atoは GitHub repo やローカルプロジェクトを controlled runtime で動かし、必要な tools/runtime を検出・準備して実行する設計です。 また、Execution Identity は単なるビルド成果物ではなく、runtime、dependency、env、filesystem view、network policy、entrypoint などを含む launch envelope を識別する考え方なので、実行基盤は「アプリをインストールする基盤」ではなく「起動条件を materialize して実行する基盤」として作るべきです。

具体的には、起動フローをこうします。

```text
1. User opens affine.capsule / capsule://...
2. Ato verifies recipe signature and source provenance
3. Ato resolves target platform
4. Ato computes execution/materialization keys
5. Ato checks local CAS store
6. Missing layers are fetched from build server or built locally
7. Ato shows consent UI:
   - source
   - recipe author
   - services
   - ports
   - network
   - state
   - required secrets
8. User approves
9. Ato creates isolated filesystem view
10. Ato starts services in dependency order
11. Ato tracks session lifecycle
12. Ato exposes app through Desktop/WebView/browser
```

`Runtime Adapter` は複数持つべきです。最初から source-native だけに絞ると AFFiNE のような大きい self-host アプリで詰まります。manifest 側も `runtime = "source" | "web" | "wasm" | "oci"` のような複数 runtime を想定しているので、実行基盤もそれに合わせて **multi-runtime orchestrator** にするのが自然です。

優先順位はこうです。

```text
v1:
  source runtime + OCI runtime + local state + service graph

v2:
  remote materialization cache
  .capsule double-click open
  recipe registry

v3:
  WASM runtime
  stronger sandbox
  cross-device session / managed runtime
```

`source runtime` はAtoの本命です。Node、Python、Rust、Bun、uv、pnpm などを Ato 管理下で解決し、ローカルの PATH や HOME を汚さずに起動する。

`OCI runtime` は現実解です。AFFiNE、Supabase系、Postgres/Redis依存アプリはまずOCIで動かせるようにする。ここは Docker 互換ではなく、Atoの service graph に OCI target をぶら下げる形にする。

`WASM runtime` は軽量ツールや安全な plugin 用。最初は不要ですが、将来の「信頼できない小物ツール実行」には向いています。

`web/static runtime` はビルド済み frontend、docs、ローカルHTML系。Ato Desktop の store / onboarding にも使える。

実行基盤の中心は **Session Manager** です。window を閉じることとプロセス終了を分離し、session を実行単位にするべきです。

```text
App window:
  表示面

Session:
  実行中プロセス
  ports
  logs
  state mounts
  readiness
  restart/stop
  execution identity

Recipe:
  どう起動するかの宣言
```

この分離がないと、OS browser で開いたアプリ、headless service、Desktop WebView のライフサイクルが崩れます。Ato Desktop の “Open Windows” も、本当は “Open Sessions” に近いモデルにした方がいいです。

セキュリティ境界は `Policy & Consent Broker` に寄せます。

```text
default deny:
  env
  network
  filesystem write
  host bridge
  secrets

explicit allow:
  required_env
  egress_allow
  state mount
  local port
  host tool bridge
```

秘密情報は `.capsule` に入れない。レシピは `OPENAI_API_KEY required` までを宣言し、実値はローカル secret store から注入する。ネットワークも deny-by-default にして、許可ドメインを consent UI に出す。環境変数も暗黙継承ではなく allowlist にする。これは manifest 仕様の方向性とも一致します。

ローカルのファイル構造はこういう形がよいです。

```text
~/.ato/
  store/
    blobs/
    sources/
    deps/
    builds/
    runtimes/
  recipes/
    affine/
  sessions/
    <session-id>/
      tmp/
      logs/
      run.json
  state/
    affine/
      postgres/
      storage/
      config/
  trust/
    recipe-authors.json
    build-servers.json
  secrets/
    local encrypted store
```

`.capsule` はこの store に import されるだけで、直接展開して直接実行しない。必ず検証、CAS登録、policy解決、session生成を通す。

Ato Build Server は「実行基盤」ではなく **materialization provider** に限定した方が安全です。

```text
Build Server does:
  dependencies を作る
  native module を build する
  build output を作る
  SBOM/provenance を署名する

Build Server does not:
  user secrets を受け取る
  user state を受け取る
  local session を実行する
```

最終的な実行はローカル。ここが重要です。Atoの価値は「クラウドでアプリを動かす」ではなく、**ローカルで動かすための実行条件を正しく揃える**ことです。

まとめると、実行基盤はこの一文にできます。

```text
Ato Runtime は、runner-authored recipe を検証し、必要な依存層を materialize し、
ローカルの隔離環境で session として実行する signed launch interpreter。
```

最初に作るべきMVPは、`source + OCI + service graph + local state + consent UI + session manager` です。これができると、AFFiNEのような大きいアプリも、Blinkoのような中規模アプリも、同じ実行基盤の上で扱えます。

---

# you asked

install -> buildが重いアプリはそこだけサーバーで事前に実行してからローカルに渡すというイメージでした

---

# chatgpt response

はい、その理解でよいです。Atoでやるべきなのは、アプリ全体をサーバーで動かすことではなく、**重い `install -> build` フェーズだけをサーバーで先に materialize して、実行可能な依存層・ビルド成果物としてローカルに渡す**ことです。

言い換えると、これは「クラウド実行」ではなく **remote build cache / remote materialization** です。

```text
local:
  recipe resolve
  source identity check
  execution plan consent
  secrets/state/network policy
  run/session lifecycle

server:
  install
  dependency build
  native module compile
  frontend build
  generated assets
  SBOM/provenance/signature
```

AFFiNEのような重いアプリなら、サーバー側でやるのは例えばこの部分です。

```text
corepack enable
yarn install --immutable
native module build
server-native build
frontend/app build
artifact packaging
```

ローカル側では、Postgres/Redis/state/secrets/network consent を解決して、サーバーから受け取った `node_modules` 相当・native module・build output を読み取り専用 layer として差し込んで起動する。これなら「初回セットアップ30分」を「artifact download + launch」にできます。

設計上は `execution_id` をそのまま remote build key にしない方がよいです。分けます。

```text
execution_id:
  実際のローカル起動条件全体
  runtime, env, fs view, network, state, entrypoint, argv, cwd

materialization_key:
  install/build に必要な部分だけ
  source hash, lock hash, build command, toolchain, platform, build env policy
```

つまりこうです。

```text
materialization_key = H(
  source_tree_hash,
  capsule/recipe hash,
  lockfile hash,
  target label,
  install command,
  build command,
  package manager identity,
  runtime/toolchain identity,
  platform profile,
  build-time env allowlist,
  lifecycle policy
)
```

サーバーはこの key に対して、次のような成果物を返す。

```text
materialized-output/
  deps.layer.tar.zst
  build.layer.tar.zst
  runtime-tools.json
  sbom.spdx.json
  provenance.json
  build.log
  signature.json
```

クライアントは署名と hash を検証して、ローカル CAS に import する。

```text
~/.ato/store/
  deps/<hash>
  builds/<hash>
  runtimes/<hash>
```

その上でローカル実行時の filesystem view はこう組む。

```text
/app/source        -> source layer
/app/node_modules  -> server materialized deps layer
/app/dist          -> server materialized build layer
/app/.state        -> local persistent state
/tmp               -> local session tmp
```

重要なのは、server が生成した layer は基本的に **read-only** にすることです。実行時の mutable state はローカル側に残す。これにより、「サーバーで作った再利用可能な成果物」と「ユーザー固有の状態」が混ざりません。

recipe 側には、phase ごとに remote materialization 可能かを宣言できるとよいです。

```toml
[targets.main]
runtime = "source"
driver = "node"
runtime_version = "22.22.3"
package_manager = "yarn@4.13.0"
install = "yarn install --immutable"
build = "yarn build"
run = "yarn start"
port = 3010

[targets.main.materialization]
install = "remote-preferred"
build = "remote-preferred"
run = "local-only"

[targets.main.materialization.outputs]
deps = ["node_modules", ".yarn/cache"]
build = ["dist", "packages/frontend/dist", "packages/backend/dist"]
```

`run = local-only` が肝です。Atoがサーバーでやるのは install/build まで。ユーザーの secrets、DB、永続 state、ネットワーク許可、ポート公開はローカルの consent 後に実行する。

この仕組みで事前実行できるものは多いです。

```text
向いている:
  npm/pnpm/yarn install
  uv pip install
  cargo build
  native addon compile
  prisma generate
  vite/next build
  static asset build
  codegen
  model/tokenizer downloadのうち固定できるもの

向いていない:
  user secrets が必要な処理
  user DB migration
  local HOME や絶対パスに依存する処理
  GPU/driver 実機検出が必要な処理
  実行時に外部APIから変動データを取る処理
```

DB migration は原則ローカルです。サーバーで作れるのは migration script や generated client までで、ユーザーの Postgres に対する `migrate deploy` はローカル session 起動前に実行するべきです。

MVPの順番はこうです。

1. **local build materialization を厳密化**  
   まずローカルで `install/build` の input digest と output layer を作れるようにする。

2. **remote substitute を追加**  
   local で build する前に、`materialization_key` で Ato Build Server / Registry に問い合わせる。

3. **public GitHub repo のみ対応**  
   private source upload は後回し。まずは `github.com/org/repo@commit` だけ。

4. **platform別 worker**  
   `linux-x86_64-glibc`, `linux-arm64`, `darwin-arm64`, `windows-x86_64` で別 materialization にする。

5. **recipe registry と接続**  
   「このAFFiNE recipeは darwin-arm64 / linux-x86_64 で materialized 済み」と表示する。

この設計なら、Atoの価値はかなり明確になります。

```text
従来:
  git clone
  install
  build
  error
  run

Ato:
  recipeを読む
  install/build済みlayerを検証して取得
  secrets/stateだけローカルで入れる
  run
```

なので、あなたのイメージは正しいです。  
Atoではこれを **phase-level remote materialization** として設計するのが一番きれいです。

---

# you asked

このイメージを実際に実行できるようにするためにはどのような設計にするのがいいと思う？ シーケンスを考えてください

従来:
  git clone
  install
  build
  error
  run

Ato:
  recipeを読む
  install/build済みlayerを検証して取得
  secrets/stateだけローカルで入れる
  run

---

# chatgpt response

良い設計は、Atoの実行パイプラインを **「ローカルで決める部分」と「サーバーで事前 materialize できる部分」に分割する**ことです。

核はこれです。

```text
install/build は remote materialization 可能
run は local-only
```

つまり、Atoはアプリをクラウドで実行しない。重い依存解決・native build・frontend build だけを事前に済ませ、ローカルでは検証済み layer を差し込んで起動する。

## 全体アーキテクチャ

```text
Ato Client
  ├─ Recipe Resolver
  ├─ Source Resolver
  ├─ Materialization Planner
  ├─ Local CAS Store
  ├─ Consent / Secret / State Manager
  └─ Session Runner

Ato Materialization Service
  ├─ Recipe Registry
  ├─ Remote CAS
  ├─ Build Scheduler
  ├─ Platform Workers
  └─ Signing / Provenance Service
```

Ato Client が常に主導権を持ちます。サーバーは `install/build済みlayerを作る・保存する・署名する` だけです。

## シーケンス 1: cache hit の理想フロー

```text
User
  |
  | open affine.capsule / ato run github.com/toeverything/AFFiNE --recipe affine
  v
Ato Client
  |
  | 1. recipe を読む
  |    - source repo
  |    - target
  |    - install command
  |    - build command
  |    - run command
  |    - required services
  |    - required secrets/state/network
  v
Source Resolver
  |
  | 2. source を commit 単位で固定
  |    github.com/toeverything/AFFiNE@<commit>
  |
  | 3. source_tree_hash を計算
  v
Materialization Planner
  |
  | 4. materialization_key を計算
  |
  | 5. local CAS を確認
  |      hit? no
  v
Ato Remote CAS
  |
  | 6. materialization_key で問い合わせ
  |      hit? yes
  |
  | 7. deps/build layers + manifest + signature を返す
  v
Ato Client
  |
  | 8. 署名・hash・platform profile を検証
  |
  | 9. local CAS に import
  |
  | 10. consent UI
  |      - source
  |      - recipe author
  |      - materialized by
  |      - network
  |      - services
  |      - secrets
  |      - state
  |
  | 11. secrets/state だけローカルで補完
  |
  | 12. filesystem view を組む
  |
  | 13. run
```

ユーザー体験としてはこうなります。

```text
Ato:
  recipeを読む
  install/build済みlayerを検証して取得
  secrets/stateだけローカルで入れる
  run
```

## シーケンス 2: cache miss の場合

cache miss なら、その場でサーバーに build request を投げます。

```text
Ato Client
  |
  | 1. materialization_key で remote CAS lookup
  v
Ato Remote CAS
  |
  | 2. miss
  v
Ato Build Scheduler
  |
  | 3. build request を登録
  |    - source_ref
  |    - source_tree_hash
  |    - recipe_hash
  |    - lock_hash
  |    - target
  |    - platform_profile
  |    - install/build commands
  |    - output contract
  v
Platform Worker
  |
  | 4. public source を fetch
  |
  | 5. isolated worker 上で install
  |
  | 6. build
  |
  | 7. output contract に従って成果物を抽出
  |    - deps layer
  |    - build layer
  |    - generated assets
  |    - native modules
  |
  | 8. host-specific path を正規化
  |
  | 9. SBOM / provenance / build log を生成
  |
  | 10. layer hash を計算
  v
Signing Service
  |
  | 11. materialization manifest に署名
  v
Remote CAS
  |
  | 12. 保存
  v
Ato Client
  |
  | 13. download
  |
  | 14. verify
  |
  | 15. local run
```

重要なのは、server が source を受け取るのではなく、まずは **public GitHub repo + commit** を自分で fetch する形にすることです。private repo / local source upload は後回しでよいです。

## materialization_key の設計

`execution_id` と `materialization_key` は分けます。

```text
execution_id:
  実際のローカル起動条件全体

materialization_key:
  install/build layer を再利用できるかを判定する key
```

`materialization_key` はこのように作ります。

```text
materialization_key = H(
  source_tree_hash,
  recipe_hash,
  lock_hash,
  target_label,
  install_command,
  build_command,
  package_manager_identity,
  runtime_identity,
  runtime_tools_identity,
  platform_profile,
  build_env_policy,
  output_contract,
  materializer_version
)
```

`execution_id` には secrets/state/network/local port/cwd などが入りますが、`materialization_key` には入れません。ここを混ぜると、ユーザーごとの実行条件でキャッシュが割れてしまいます。

## layer の成果物形式

サーバーが返すものは、単なる tarball ではなく、検証可能な materialized layer にします。

```text
materialized-output/
├── materialization.manifest.json
├── deps.layer.tar.zst
├── build.layer.tar.zst
├── sbom.spdx.json
├── provenance.json
├── build.log
└── signature.json
```

manifest は例えばこうです。

```json
{
  "schema_version": 1,
  "materialization_key": "blake3:...",
  "source_tree_hash": "blake3:...",
  "recipe_hash": "blake3:...",
  "platform": "linux-x86_64-glibc",
  "runtime": {
    "node": "22.22.3",
    "yarn": "4.13.0"
  },
  "layers": [
    {
      "kind": "deps",
      "path": "deps.layer.tar.zst",
      "hash": "blake3:..."
    },
    {
      "kind": "build",
      "path": "build.layer.tar.zst",
      "hash": "blake3:..."
    }
  ],
  "outputs": {
    "deps": ["node_modules", ".yarn/cache"],
    "build": ["dist", "packages/frontend/dist"]
  }
}
```

## recipe 側の記述

recipe では、どの phase を remote materialization できるかを宣言します。

```toml
schema_version = "0.3"
name = "affine"
version = "0.1.0"
type = "app"
default_target = "main"

[source]
kind = "github"
repo = "toeverything/AFFiNE"
ref = "commit-sha-required"

[targets.main]
runtime = "source"
driver = "node"
runtime_version = "22.22.3"
package_manager = "yarn@4.13.0"
install = "yarn install --immutable"
build = "yarn build"
run = "yarn start"
port = 3010

[targets.main.materialization]
install = "remote-preferred"
build = "remote-preferred"
run = "local-only"

[targets.main.materialization.outputs]
deps = ["node_modules", ".yarn/cache"]
build = ["dist", "packages/frontend/dist"]

[secrets]
required = []

[state.storage]
kind = "filesystem"
durability = "persistent"
mount = "/app/storage"

[network]
egress_allow = []
```

`run = "local-only"` を明示するのが大事です。Atoの信頼境界をここで固定します。

## ローカル実行時の filesystem view

取得した layer は直接実行しません。Atoの local store に import して、起動時に読み取り専用で差し込みます。

```text
/session/<id>/root
├── app/source        -> source layer
├── app/node_modules  -> deps layer, read-only
├── app/dist          -> build layer, read-only
├── app/storage       -> local persistent state
└── tmp               -> session tmp
```

mutable なものは必ず local state 側に逃がします。server-generated layer を実行中に書き換えさせない方がよいです。

## install/build の境界

remote に寄せてよいもの。

```text
npm/pnpm/yarn install
uv pip install
cargo build
native addon compile
frontend build
Prisma client generate
codegen
static asset generation
```

local に残すべきもの。

```text
secrets 注入
DB migration
user state 初期化
local port binding
host bridge
runtime network permission
actual process launch
```

DB migration は注意が必要です。migration file や generated client は server で作れますが、ユーザーの Postgres に対する migration 適用は local の起動前 hook にするべきです。

## 実装順

最初から remote build server を作るより、順序はこうです。

1. **Local phase materialization**
   まずローカルで `install/build` の input digest を計算し、output layer をCAS保存できるようにする。

2. **Output contract**
   recipe に `outputs.deps` / `outputs.build` を書けるようにする。これがないと、何を再利用可能 layer として保存するか決められません。

3. **Local cache hit 実行**
   同じ `materialization_key` なら install/build をskipし、local layerを差し込んでrunする。

4. **Remote CAS lookup**
   local miss 時に remote に問い合わせる。ただし最初は read-only cache でよいです。

5. **Remote worker build**
   cache miss 時に public GitHub repo を worker が materialize する。

6. **署名・provenance**
   worker output に署名し、client 側で必ず検証する。

7. **Desktop consent UI**
   「誰のsourceか」「誰のrecipeか」「誰がmaterializeしたか」を分けて表示する。

## 最小MVPの完成条件

AFFiNEまで行く前に、まずは小さな Node/Vite アプリでこれを通すとよいです。

```text
初回:
  ato run github.com/example/vite-app
  remote miss
  server build
  layer download
  run

2回目:
  ato run github.com/example/vite-app
  local CAS hit
  install/build skip
  run

別ユーザー:
  ato run github.com/example/vite-app
  remote CAS hit
  layer download
  run
```

ここまで通れば、Atoの体験はかなり変わります。

```text
従来:
  clone → install → build → error → run

Ato:
  recipe → verified materialized layers → local secrets/state → run
```

設計名としては **Phase-level Remote Materialization** が一番正確です。Atoの実行基盤としては、`install/build` を cacheable phase、`run` を local session phase として分離するのがよいです。