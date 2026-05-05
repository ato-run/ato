# Run

## Overview

`ato run` は、ローカルディレクトリ、GitHub リポジトリ、Store 参照、share URL を
「いま動かす」ための front door である。Ato の公開ドキュメントでは、まずこの
ページから run の振る舞いを理解し、詳細契約は RFC に降りる。

## How it works

大きな流れは次の通り。

1. 入力を正規化する
2. `capsule.toml` または preview metadata をもとに runtime を決める
3. 必要な tool/runtime を解決する
4. lock と policy を使って実行計画を固める
5. 制御された環境で起動する

詳細な CLI surface と routing は
[`ATO_CLI_SPEC.md`](rfcs/accepted/ATO_CLI_SPEC.md) と
[Core Architecture](core-architecture.md) が正本に近い説明になる。

## Specification

- `ato run` MUST treat execution as ephemeral, not as persistent installation.
- `ato run` MUST resolve local path, GitHub repo, scoped Store reference, and share URL.
- required env MUST fail closed before process launch.
- runtime resolution SHOULD prefer pinned runtimes and tools recorded by lock state.
- `capsule.toml` is the primary authoring contract for local projects.

根拠:

- [`Repository README`](https://github.com/ato-run/ato/blob/main/README.md)
- [`rfcs/accepted/ATO_CLI_SPEC.md`](rfcs/accepted/ATO_CLI_SPEC.md)

## Design Notes

`run` を front door に固定する理由は、対象ごとに mental model を増やさないため。
「何を開くか」ではなく「同じ handle でどう実行するか」を優先する。
