---
title: "Foundation Profile And Placement"
status: accepted
date: "2026-02-17"
author: "@egamikohsuke"
ssot: []
related: []
---

# Foundation Profile And Placement

**目的:** `foundation` に含める system declaration と、`state` に属する placement/live condition の境界を定義する。
**前提:** Ato は `capsule / foundation / resource / state` の 4 分類と single-CAS model を採る。

この仕様における live state は canonical snapshot file を正本 API とし、DB は index に留める。

## 1. 要約

Ato では、remote GPU や外部実行基盤を扱うために、`node` を大きな第一級概念として導入するよりも、次の 2 層に分けて表現する。

- `foundation profile`
  実行基盤の宣言的・比較的安定した条件
- `placement state`
  その条件を今どの実体が提供できるかという live state

この分離により、capsule は「どのマシンで動くか」ではなく「どの foundation profile が必要か」を宣言できる。

さらに version 解決は次の 3 層で扱う。

- `foundation requirements`
  capsule が要求する version constraint
- `foundation inventory`
  現在その実体で利用可能な exact version 集合
- `foundation lock`
  実際に選ばれた exact version

## 2. 一枚表

| 項目 | 属する分類 | 性質 | 例 | CAS格納 | 更新方法 |
| --- | --- | --- | --- | --- | --- |
| OS / arch / ABI | `foundation` | 宣言的、比較的安定 | `linux`, `x86_64`, `glibc-2.39` | Yes | 新 profile/version |
| runtime / tool / engine artifact | `foundation` | immutable artifact | `python=3.12.2`, `uv=0.4.19`, `nacelle=0.9.0` | Yes | 新 artifact/version |
| accelerator API / driver contract | `foundation` | 宣言的、比較的安定 | `cuda=12.4`, `metal`, `rocm` | Yes | 新 profile/version |
| version constraint | capsule requirement | 宣言的 | `python >=3.11,<3.13`, `uv ^0.4` | Yes | manifest/lock 更新 |
| available version set | `state` | 動的 | `python=[3.10.0,3.11.0,3.11.1,3.12.2]` | No | in-place mutate |
| resolved exact version | lock | 実行時固定 | `python=3.12.2` | Yes | resolve し直し |
| GPU が今空いているか | `state` | 動的 | `available=true` | No | in-place mutate |
| free VRAM / utilization | `state` | 動的 | `vram_free=18GiB` | No | in-place mutate |
| queue length / lease / reservation | `state` | 動的 | `lease_id=...` | No | in-place mutate |
| remote endpoint reachability | `state` | 動的 | `reachable=true`, `rtt_ms=23` | No | in-place mutate |

## 3. Foundation Profile

`foundation profile` は、ある実行基盤が満たす capability contract を immutable に表したものとする。

最低限、次を持つ。

- `profile`
- `os`
- `arch`
- `abi` または `libc`
- `accelerators`
- `features`
- `device_classes`
- `extensions`
- `compatibility_policy`
- `signatures`
- `provenance`

例:

```json
{
  "kind": "foundation-profile",
  "profile": "linux-x86_64-cuda12",
  "os": "linux",
  "arch": "x86_64",
  "abi": "glibc-2.39",
  "accelerators": {
    "gpu": true,
    "apis": ["cuda"],
    "cuda_driver": "12.4"
  },
  "features": [
    "sandbox:nacelle",
    "fs:overlay",
    "device:hotplug"
  ],
  "device_classes": ["gpu", "display", "camera"],
  "extensions": {
    "desktop.gpu.external-egpu": false
  },
  "compatibility_policy": {
    "os.patch": "compatible",
    "os.minor": "revalidate",
    "os.major": "relock",
    "driver.patch": "compatible",
    "driver.major": "relock",
    "feature.removal": "fail_closed"
  }
}
```

profile は capability を表し、exact version の最終選択までは表さない。
`python=3.12.2` のような具体値は artifact / inventory / lock で扱う。

### 3.1 Design Rule

foundation は version を主語にするのではなく、feature / capability を主語にして表現する。

良い例:

- `gpu-api:cuda`
- `device:hotplug`
- `sandbox:nacelle`
- `display:hdr`

version は compatibility 判定や exact lock に必要だが、要求面の主語としては二次的に扱う。

## 4. Foundation Requirements / Inventory / Lock

### 4.1 Requirements

capsule は foundation を exact version で固定するのではなく、まず constraint で要求する。

例:

```json
{
  "foundation_requirements": {
    "profile": "linux-x86_64-cuda12",
    "engines": {
      "nacelle": "^0.9"
    },
    "runtimes": {
      "python": ">=3.11,<3.13"
    },
    "tools": {
      "uv": "^0.4"
    }
  }
}
```

### 4.2 Inventory

inventory は、現在ある placement candidate が持っている exact version 集合である。

例:

```json
{
  "foundation_inventory": {
    "drivers": {
      "nvidia": ["550.54.15"]
    },
    "devices": {
      "gpu": [
        { "vendor": "nvidia", "model": "RTX 4090", "vram": "24GiB" }
      ]
    },
    "engines": {
      "nacelle": ["0.9.0"]
    },
    "runtimes": {
      "python": ["3.10.0", "3.11.0", "3.11.1", "3.12.2"]
    },
    "tools": {
      "uv": ["0.4.19"]
    }
  }
}
```

inventory は placement candidate の live state に属し、CAS の immutable profile には含めない。
resolver は inventory を hidden DB から直接読むのではなく、inventory snapshot file を入力として扱う。

OS upgrade や新しいデバイス接続、driver 更新は profile そのものではなく、inventory refresh と profile compatibility check を通じて反映する。

### 4.3 Lock

install / run の時点では、requirements と inventory の交差から 1 つを選び、exact version に lock する。

例:

```json
{
  "foundation_lock": {
    "profile": "linux-x86_64-cuda12",
    "engines": {
      "nacelle": "0.9.0"
    },
    "runtimes": {
      "python": "3.12.2"
    },
    "tools": {
      "uv": "0.4.19"
    }
  }
}
```

### 4.4 Resolution Rule

複数 version が候補にある場合の既定ルールは次とする。

1. 既存 lock があれば最優先
2. exact pin が要求されていればそれを優先
3. range 指定なら条件を満たす最新安定版を選ぶ
4. placement candidate 上に既に存在するものを優先してもよい
5. 条件を満たすものがなければ fetch
6. それでも満たせなければ fail-close

### 4.5 Upgrade And Extension Handling

OS の version update や desktop PC 上の新しい device / driver 追加は、次のように扱う。

1. host の foundation inventory を refresh する
2. foundation profile の compatibility policy に照らして互換性を判定する
3. 互換なら既存 lock を維持する
4. 再検証が必要なら revalidate する
5. relock が必要なら exact version を再解決する
6. feature が失われた場合は fail-close する

## 5. Placement State

`placement state` は、foundation profile を満たす実体が、今この瞬間に実行を受けられるかを示す live state である。

例:

- online/offline
- reachable/unreachable
- battery / thermal pressure
- queue length
- current leases
- free VRAM
- measured latency

これは `state/placements/current.json` や `state/placements.db` に属し、CAS には格納しない。
canonical API は `current.json` のような file projection とする。

## 6. Capsule 側の要求

capsule は host identity を要求せず、foundation profile と version constraint を要求する。

例:

```toml
[targets.infer]
runtime = "service"
driver = "remote-inference"

[targets.infer.requires.foundation]
profile = "linux-x86_64-cuda12"
python = ">=3.11,<3.13"
engine = "^0.9"
```

この要求に対し、scheduler は current placement state を見て、条件を満たす実体を選ぶ。

## 7. Scheduler の責務

scheduler は次の順で判断する。

1. capsule が要求する foundation profile を読む
2. requirements を読む
3. local foundation inventory が満たせるか判定する
4. 満たせない場合、placement state を見て他の実体を探す
5. inventory と requirements の交差から exact version を選ぶ
6. foundation lock を確定する
7. 実行先を決める
8. session lease を作る

つまり、placement の決定は `foundation` と `state snapshot` の join である。

## 8. Directory への写像

```text
~/.ato/
├── refs/
│   └── foundation/
│       ├── profiles/<profile>.json
│       └── artifacts/<name>/<version>.json
└── state/
    ├── foundation-inventory.db
    ├── placements.db
    ├── leases.db
    └── sessions.db
```

## 9. Invariants

- `foundation profile` は immutable である
- foundation は OS / driver / device の将来拡張を受け止める宣言層である
- exact version の集合は profile ではなく inventory で扱う
- capsule は foundation をまず constraint で要求する
- install/run 時に exact version を lock する
- `placement state` は mutable である
- capsule は host を要求せず foundation profile を要求する
- placement は profile と live state から決まる
- remote GPU の可用性は foundation ではなく state に属する

## 10. まとめ

外部 GPU や別実体での実行を扱う場合でも、Ato は「どの node か」を第一義にせず、「どの foundation profile が必要か」を第一義に置くべきである。

その上で、どの exact version を使うかは requirements / inventory / lock の 3 層で決め、現在その profile をどの実体が提供できるかは placement state が決める。

この分離により、Ato は host-centric ではなく capability-centric な execution model を保てる。
また foundation を capability-first で設計することで、OS アップデートや desktop ハードウェア拡張を schema を壊さず吸収できる。
