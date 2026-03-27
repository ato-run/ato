# Ticket 11: Single-File Script Input

- Status: Draft
- Priority: P1
- Depends on: 02, 04

## Goal

`ato run foo.py` のような単一ファイル入力を、lock-first を壊さずに source-only path へ統合する。

## Decision

単一ファイルは manifest-less special case として直接実行しない。
代わりに、attempt-scoped な virtual workspace を materialize し、その workspace から既存の shared source inference を走らせる。

## Rules

- authoritative input として受けるのは、当面 `*.py` のみ
- resolver は `ExplicitInputKind::SingleScript` として受理する
- resolver の出力は `ResolvedSourceOnly` を維持しつつ、script metadata を付与する
- run materialization は script を `.tmp/ato-single-python-*` の virtual workspace へコピーする
- Python script は `main.py` として正規化し、PEP 723 があれば `pyproject.toml` と `requirements.txt` を合成する
- `uv lock` を virtual workspace で実行し、既存の `source/python + uv.lock` 実行モデルに接続する
- cleanup は attempt cleanup scope に委譲する

## Non-Goals

- `ato init foo.py` の durable workspace 化
- TypeScript / Ruby 単一ファイル run
- PEP 723 以外の Python inline metadata 形式

## Rationale

- shared source inference は directory-shaped evidence を前提にしている
- current run/preflight は `source/python` に `uv.lock` を要求する
- したがって最小実装は、single file を temporary project に昇格して既存 path に載せるのが最小破壊

## Follow-ups

- `ato init foo.py` で durable workspace を生成する
- TypeScript single-file は Deno/Node selection と lock strategy を別途 ADR 化する
- Ruby single-file は runtime/driver surface を先に execution model へ追加する
