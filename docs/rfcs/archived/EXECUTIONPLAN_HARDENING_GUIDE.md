# ExecutionPlan Hardening Guide

**Status:** Informative (Implementation Guide)  
**Last Updated:** 2026-02-23

本書は実装手段の具体例を記載する。規範契約は `EXECUTIONPLAN_ISOLATION_SPEC.md` を優先する。

## 1. Deno lifecycle script 抑止

- 例: `--node-modules-dir=false`

## 2. uv source build 抑止

- 例: `uv sync --only-binary :all:`

## 3. CAS atomic publish

- 例: `.tmp` で検証後に `rename(2)`

## 4. Runtime update lock

- 例: `~/.capsule/runtimes/` 更新時に `flock(2)`

## 5. ELF pre-flight

- 例: `PT_INTERP` / `DT_NEEDED` / `DT_VERNEED` を静的解析

## 6. Secret handoff

- 例: `pipe(2)` / `memfd_create(2)`
