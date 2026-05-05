# Glossary

## Overview

This is the minimal public glossary for terms that appear in the current docs
and code. It is intentionally shorter than the older internal glossary and
tracks the current implementation first.

## How it works

| Term | Current meaning |
|---|---|
| **Capsule** | The execution unit described by `capsule.toml` and routed through one of the supported runtime kinds |
| **`capsule.toml`** | The main authoring manifest surface for local projects |
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
