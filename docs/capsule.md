# Capsule

## Overview

Capsule は、Ato が扱う対象を統一して表現する実行単位である。app、tool、service
を別物として増やすのではなく、同じ宣言モデルの上で扱う。

## How it works

Capsule は `capsule.toml` を中心に記述される。

- トップレベルで名前、型、既定 target を定義する
- `[targets.<label>]` で runtime ごとの起動契約を定義する
- 必要なら dependency や isolation policy を追加する

実際の manifest 契約と format の厳密な定義は RFC にある。

## Specification

- A capsule MUST be declared through `capsule.toml`.
- A manifest MUST declare at least one runnable target.
- `schema_version`, `name`, `version`, `type`, and `default_target` MUST satisfy the current manifest contract.
- runtime-specific fields MUST follow the accepted manifest and format specs.

根拠:

- [`rfcs/accepted/CAPSULE_SPEC.md`](rfcs/accepted/CAPSULE_SPEC.md)
- [`rfcs/accepted/CAPSULE_FORMAT_V2.md`](rfcs/accepted/CAPSULE_FORMAT_V2.md)
- [`rfcs/accepted/CAPSULE_DEPENDENCY_CONTRACTS.md`](rfcs/accepted/CAPSULE_DEPENDENCY_CONTRACTS.md)

## Design Notes

Capsule という単位を維持する理由は、特例を増やす代わりに共通モデルを育てるため。
宣言、解決、実行、共有を同じ輪郭で扱えることが Ato の一貫性になる。
