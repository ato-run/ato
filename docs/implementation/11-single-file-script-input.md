# Ticket 11: Single-File Script Input

- Status: In Progress
- Priority: P1
- Depends on: 02, 04

## Goal

`ato run foo.py` や `ato init foo.ts` のような単一ファイル入力を、lock-first を壊さずに source-only path と durable workspace materialization へ統合する。

## Decision

単一ファイルは manifest-less special case として直接実行しない。
代わりに、attempt-scoped な virtual workspace を materialize し、その workspace から既存の shared source inference を走らせる。

## Rules

- authoritative input として受けるのは、当面 `*.py`, `*.ts`, `*.tsx`, `*.js`, `*.jsx`
- resolver は `ExplicitInputKind::SingleScript` として受理する
- resolver の出力は `ResolvedSourceOnly` を維持しつつ、script metadata を付与する
- run materialization は script を language-specific な `~/.ato/cache/source-inference/single-script-cache/<lang>-<hash>/` virtual workspace へコピーする
- Python script は `main.py` として正規化し、PEP 723 があれば `pyproject.toml` と `requirements.txt` を合成する
- `uv lock` を virtual workspace で実行し、既存の `source/python + uv.lock` 実行モデルに接続する
- TypeScript script は `main.ts` または `main.tsx` として正規化し、最小 `deno.json` と `deno.lock` を生成して既存の `source/deno` 実行モデルに接続する
- `.tsx` は `compilerOptions.jsx = "react-jsx"` を生成し、`@jsxImportSource ...` pragma があれば `jsxImportSource` に反映する
- JavaScript script は `main.js` または `main.jsx` として正規化し、最小 `deno.json` と `deno.lock` を生成して既存の `source/deno` 実行モデルに接続する
- `.jsx` は `compilerOptions.jsx = "react-jsx"` を生成し、`@jsxImportSource ...` pragma があれば `jsxImportSource` に反映する
- `ato init foo.ts` / `foo.tsx` は workspace root に `main.ts` または `main.tsx` と `deno.json` と `deno.lock` を durable materialization してから canonical lock を生成する
- `ato init foo.js` / `foo.jsx` は workspace root に `main.js` または `main.jsx` と `deno.json` と `deno.lock` を durable materialization してから canonical lock を生成する
- `ato init foo.py` は workspace root に `main.py` と `pyproject.toml` と `uv.lock` を durable materialization してから canonical lock を生成する
- cleanup は attempt cleanup scope に委譲する

## Non-Goals

- Node fallback / Ruby 単一ファイル run
- PEP 723 以外の Python inline metadata 形式

## Rationale

- shared source inference は directory-shaped evidence を前提にしている
- current run/preflight は `source/python` に `uv.lock` を要求する
- したがって最小実装は、single file を temporary project に昇格して既存 path に載せるのが最小破壊

## Follow-ups

- JSX runtime / compiler option inference を `@jsxRuntime` など追加 pragma まで広げる
- Node fallback を selection gate 付きで追加する
- Ruby single-file は runtime/driver surface を先に execution model へ追加する
