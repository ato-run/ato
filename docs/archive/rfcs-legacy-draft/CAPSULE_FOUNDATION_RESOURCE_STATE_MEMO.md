# Capsule / Foundation / Resource / State

**ステータス:** Draft  
**目的:** Ato におけるソフトウェア資産とデータの 4 分類を、判断基準と運用ルールが一目で分かる形で定義する。  
**前提:** local storage model は single-CAS + refs + gcroots + materialized + state を採り、重要な判断は file-first / snapshot-first に行う。

## 1. 一枚表

| 分類 | 定義 | 代表例 | Immutable | CAS格納 | 共有単位 | GC root | 更新の考え方 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `capsule` | 実行契約そのもの。manifest と payload を含むアプリケーション単位 | app, tool, service, web app, CLI capsule | Yes | Yes | capsule version | `installed`, `preview`, `session` | 新 version を作る |
| `foundation` | capsule の外部にある共通基盤。複数 capsule から再利用される実行土台と、その system declaration | `node`, `python`, `uv`, `nacelle`, `ffmpeg`, `wasmtime`, OS/ABI/GPU profile, driver/device capability | Yes | Yes | foundation artifact/version, foundation profile, foundation lock | `installed`, `pinned`, `system`, `session` | constraint で要求し、exact version に lock |
| `resource` | app 固有でもよいが、immutable dependency として扱える大きなデータ | AI model, tokenizer, LoRA配布物, embedding index snapshot, static media pack, dataset | Yes | Yes | resource version / digest | `installed`, `pinned`, `session` | 新 version を作る |
| `state` | 実行により変化する mutable な実機状態。ユーザーまたは session に属する | DB, user uploads, generated videos, inference cache, workdir, settings, checkpoint-in-progress | No | No | app scope / user scope / session scope | GC root ではなく ownership / lifecycle で管理 | その場で mutate |

## 2. 最重要の判断軸

この 4 分類は「アプリ固有かどうか」ではなく、次の 2 軸で決める。

1. 実行契約そのものか
2. immutable dependency か、mutable state か

判断順は次の通り。

1. それは capsule manifest と一緒に version される実行契約か  
   Yes -> `capsule`
2. それは複数 capsule が共有する外部基盤か  
   Yes -> `foundation`
3. それは immutable dependency として digest で固定できるか  
   Yes -> `resource`
4. それ以外で、実行中または利用中に変化するか  
   Yes -> `state`

## 3. 各分類の意味

### 3.1 `capsule`

`capsule` は Ato における第一級オブジェクトであり、実行契約の単位である。

- manifest
- payload tree
- package
- signature / provenance

を持つ。

重要なのは、`capsule` は「アプリ本体」であって、巨大な model や dataset を抱え込む必要はないこと。そうした外部依存は `resource` として切り出す。

### 3.2 `foundation`

`foundation` は capsule の外部依存だが、単なる app-specific asset ではなく、実行のための共有基盤である。

- language runtime
- package / toolchain helper
- sandbox / engine
- 実行補助バイナリ
- OS / ABI / accelerator API の宣言
- 実行可能性を判断する system profile
- host feature / device class / driver compatibility
- 将来の OS update / device hotplug / driver update を受け止める拡張点

を含む。

現時点では `runtime / tool / engine` を path で細分化する必要はなく、`foundation` 配下にまとめた上で metadata の `kind` で区別すればよい。  
ただし、`foundation` が表すのは宣言的で比較的安定した条件までであり、現在の可用性や割当状態は `state` に属する。  
また、exact version の最終決定は profile ではなく `requirements / inventory / lock` の 3 層で扱う。  
要求面では version より capability / feature を主語にし、OS update や hardware 拡張を壊れにくくする。

### 3.3 `resource`

`resource` は app 固有でもよいが、mutable state ではなく immutable dependency として配布・共有・pin 可能なデータである。

代表例:

- AI model weights
- tokenizer
- adapter / LoRA の配布版
- embedding index snapshot
- static media pack
- 学習済み dataset snapshot
- read-only 参照データ

`resource` は capsule payload に埋め込む必要はなく、dependency として参照されるべきである。

### 3.4 `state`

`state` は mutable であり、CAS object にしてはならない。

代表例:

- DB
- user uploads
- generated output
- watch history
- local preferences
- in-progress fine-tune checkpoints
- inference intermediate
- transcode cache
- placement / lease / availability
- free VRAM, queue length, thermal pressure

`state` は object graph の一部ではなく、ownership と lifecycle に従って管理される。

## 4. 境界事例

### 4.1 AI model

- 配布される重みファイル -> `resource`
- 推論実行に必要な runtime (`python`, `uv`, `cuda helper`) と GPU/OS profile、driver/device capability -> `foundation`
- そのモデルを使う app -> `capsule`
- ユーザーが生成した adapter や fine-tune 途中成果物 -> `state`
- 完成して再配布可能になった adapter -> `resource`
- 現在どの GPU がこの workload を引き受けられるか -> `state`

### 4.2 動画データ

- sample video pack -> `resource`
- 学習用 read-only corpus -> `resource`
- app 内蔵 demo asset -> 小さければ `capsule`、大きければ `resource`
- ユーザー録画動画 -> `state`
- transcode 済み cache -> `state` か `cache`。immutable dependency ではない

### 4.3 Python app

- app code -> `capsule`
- `python@3.12` / `uv` -> `foundation`
- 配布済み base model -> `resource`
- sqlite db / local uploaded files -> `state`

## 5. Storage への写像

| 分類 | `refs/` | `objects/` | `gcroots/` | `materialized/` | `state/` |
| --- | --- | --- | --- | --- | --- |
| `capsule` | `refs/capsules/...` | Yes | Yes | Yes | No |
| `foundation` | `refs/foundation/...` | Yes | Yes | Yes | No |
| `resource` | `refs/resources/...` | Yes | Yes | 必要なら Yes | No |
| `state` | No | No | No | No | Yes |

## 6. 更新ルール

### 6.1 Immutable classes

`capsule`, `foundation`, `resource` は immutable class である。

- in-place update しない
- digest が変われば別 object
- version / channel / alias は `refs/` 側で切り替える

### 6.2 Mutable class

`state` は mutable class である。

- app または user scope に紐づく
- session 中に変化してよい
- ownership と lifecycle で管理する
- GC ではなく policy / app uninstall / user action で整理する

## 7. CLI / UX への含意

この 4 分類を採ると、CLI surface は次の mental model に揃えられる。

- `ato install` は主に `capsule` の installed root を張る
- `ato run` は `capsule` に加えて必要な `foundation` と `resource` を session root に入れて起動する
- `ato run` の placement は `foundation profile` 要求と `state` の live condition の組み合わせで決まる
- `foundation` の version は通常 constraint で要求し、run/install 時に exact version を lock する
- `ato run` は manifest から直接 launch せず、`graph lock -> placement lock -> session` の順で進む
- `ato pin` は `capsule`, `foundation`, `resource` のいずれにも使える
- `ato uninstall` は `capsule` の installed root を外す
- `state` は install/uninstall より ownership cleanup の問題として扱う

## 8. 推奨 metadata

### 8.1 `foundation`

- `kind`: `runtime | tool | engine`
- `name`
- `version`
- `platform`
- `profile`
- `requirements`
- `lock`
- `os`
- `arch`
- `abi`
- `accelerators`
- `features`
- `device_classes`
- `drivers`
- `extensions`
- `compatibility_policy`
- `signatures`
- `provenance`

### 8.2 `resource`

- `kind`: `model | tokenizer | adapter | dataset | media-pack | index | asset-pack`
- `name`
- `version`
- `format`
- `license`
- `origin`
- `runtime_requirements`
- `placement_constraints`

## 9. まとめ

Ato の local system を設計するうえで、扱う対象は次の 4 つに整理するのが最も安定する。

- `capsule`
- `foundation`
- `resource`
- `state`

このうち CAS に入るのは `capsule`, `foundation`, `resource` であり、`state` は CAS に入らない。  
特に `foundation` は static な system declaration を含むが、現在の placement や availability は `state` に残す。  
複数 version が共存する場合でも、capsule は constraint を宣言し、resolver が inventory を見て exact version に lock すればよい。
さらに foundation を capability-first に設計することで、OS の version update や desktop PC の新しい device / driver 接続にも拡張可能になる。
また、live state は canonical snapshot file を正本 API とし、hidden DB を直接判断に使わない。

設計上の最重要原則は次の一文に要約される。

**app 固有かどうかではなく、実行契約か、共有基盤か、immutable dependency か、mutable state かで分類する。**
