# Glossary

## Overview

This is the minimal public glossary for terms that appear in the current docs
and code. It is intentionally shorter than the older internal glossary and
tracks the current implementation first.

## How it works

| Term | Current meaning |
|---|---|
| **Source** | Raw input material for a recipe: Git repository, local directory, source snapshot, generated build output, or declared artifact |
| **Recipe** | The executable interpretation of source inputs. A recipe defines how source inputs are arranged, built, configured, permissioned, and launched. The Store shares recipes, not source code or app binaries |
| **`capsule.toml`** | The local file format for an Ato recipe. Not necessarily one-to-one with a repository — a repository may have many recipes |
| **Execution** | A resolved launch produced from source inputs, a recipe, and the user environment |
| **Execution Identity** | A content-addressed fingerprint of the resolved launch world, including recipe snapshot, source snapshots, runtime, environment, filesystem grants, network policy, capability policy, and entrypoint |
| **Session** | A managed, running or reusable execution |
| **Capsule** | A runnable unit materialized from a recipe. In current docs, prefer "recipe" when discussing authoring, sharing, review, or Store entries |
| **Target** | A named execution surface under `[targets.<label>]` |
| **`default_target`** | The target selected when the caller does not specify one |
| **Runtime kind** | The routed runtime family: `source`, `wasm`, `oci`, or `web` |
| **Execution descriptor** | The routed execution plan built from a manifest or lock input |
| **`ato.lock.json`** | The authoritative lock-backed execution input when present and selected |
| **Nacelle** | The current execution engine implementation used through the internal JSON-over-stdio contract |
| **Provider toolchain** | The language-specific runtime tooling used inside execution, such as `uv`, `node`, or `deno` |
| **Required env** | Environment variables that must be present before launch; missing values fail closed |
| **Dependency contract** | A dependency relationship declared under `[dependencies.<alias>]` with parameters, credentials, and exported values |
| **Runtime exports** | Runtime-only dependency outputs injected into the consumer environment and excluded from identity |
| **Sandbox grant** | Explicit host filesystem access granted through flags such as `--read`, `--write`, and `--read-write` |
| **Execution receipt** | The structured document that records the launch envelope for a run |
| **Execution ID** | The canonical digest of the launch identity, used to address execution receipts |
| **Connected Runner** | A machine enrolled under an Ato account (`ato runner enroll`) that heartbeats to the control plane and executes dispatched run leases sandboxed. See [Connected Runner](runner.md) |
| **Run lease** | A dispatched run claimed by a Connected Runner; the runner reports status, readiness, and stop acknowledgements against the lease |
| **Ready-State snapshot** | A sealed, content-addressed Firecracker artifact of a capsule already booted and probe-verified; restoring it serves the app without re-running setup. See [Snapshot v1 Compatibility](snapshot-v1-compatibility.md) |

## Specification

- glossary terms SHOULD prefer current code and public behavior over historical wording
- public docs SHOULD use these terms consistently across topic pages
- if docs and code diverge, the code is authoritative

References:

- [Capsule](capsule.md)
- [Run](run.md)
- [Sandbox](sandbox.md)
- [Execution Identity](execution-identity.md)

## Design Notes

The older glossary tried to be exhaustive and drifted. This version stays small
on purpose so it can track the implementation without becoming another archive.
