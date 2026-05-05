# Execution Identity

## Overview

Execution Identity は、Ato が「この起動条件は同じか」を識別するための launch
envelope の identity である。単なる source hash ではなく、runtime と環境も含む。

## How it works

execution identity は起動前に計算される。対象は source tree だけではなく、次の
ような launch condition 全体である。

- source tree
- dependency derivation
- runtime identity
- environment closure
- filesystem view
- network policy
- capability policy
- entry point / argv / working directory

desktop 側では、receipt-aware launch で得られた `execution_id` を surface metadata に
保持し、`~/.ato/executions/<execution_id>/receipt.json` と対応づける。

## Specification

- `execution_id` MUST identify launch conditions, not just source content.
- runtime identity MUST include both declared and resolved runtime identity.
- execution receipts MUST be addressable by `execution_id`.
- secret values MUST NOT be recorded directly in receipts.

根拠:

- [`rfcs/draft/beyond-reproducible-build.ja.md`](rfcs/draft/beyond-reproducible-build.ja.md)
- [`crates/ato-desktop/src/orchestrator.rs`](https://github.com/ato-run/ato/blob/main/crates/ato-desktop/src/orchestrator.rs)

## Design Notes

「同じコード」だけでは再現性を表せない。Ato が残したいのは build artifact の一致
ではなく、どの world を観測する起動だったかという実行条件の輪郭である。
