---
title: "ADR-009: Artifact build producers accept build-safe requests"
status: draft
date: "2026-05-22"
author: "@egamikohsuke"
related:
  - "ADR-008-phase-materialization-boundaries.md"
---

# ADR-009: Artifact build producers accept build-safe requests

## Context

The application lifecycle design makes installed applications output-first and
prefers remote builds for public repositories with a local fallback. That model
needs an artifact producer contract before Ato adds remote build submission,
worker scheduling, or install roots.

ADR-008 defines current phase build-output layer mechanics: a local
materialization key, an immutable output blob, optional file-backed remote CAS
lookup/export, and local projection before run. Those mechanics are useful, but
the worker boundary must use lifecycle vocabulary. A future producer generates a
candidate immutable artifact revision. It does not allocate an installed app,
launch profile, local session, or runtime process.

## Decision

### Artifact build identity

`artifact_build_id` is the canonical reusable build artifact cache identity for
artifact producer requests and responses.

```text
artifact_build_id != execution_id
```

`execution_id` describes a launch envelope. It may include launch-time policy,
filesystem view, environment closure, entrypoint, argv, cwd, and other
pre-execution conditions. An artifact build identity stays stable across local
session IDs, effective ports, runtime process IDs, and other launch-only state.

The current build-output phase materialization implementation already uses
`materialization_key` for local and file-backed remote CAS lookup. That key
remains an implementation-level compatibility field in v0 producer contracts.
It is not the ADR-level identity for reusable artifact builds.

### Build-safe worker input

An artifact build producer request carries only inputs needed to derive and
validate reusable build outputs:

- pinned public source identity or a future immutable source snapshot identity,
- source-tree, recipe, and optional lock digests,
- target label and build phase,
- build command identity,
- declared output contract digest and outputs,
- structured platform and toolchain identity,
- materializer and projection version labels,
- an explicit build producer policy.

The structured platform profile separates OS, architecture, ABI, libc/runtime
ABI, and native-addon compatibility boundaries. Cross-platform or
native-boundary reuse is a distinct artifact build identity.

The v0 source contract accepts public GitHub commits. GitHub branches, tags,
`latest`, private repository grants, and local source upload are not v0 worker
input.

### Excluded worker input

A build worker never receives:

- secret values,
- secret references when the reference implies a user grant,
- user persistent state,
- launch profiles,
- `install_profile_key`,
- `capsule_instance_key`,
- local session IDs,
- allocated local ports,
- HOME absolute paths,
- runtime process IDs,
- consent records.

Those concepts belong to install instances, launch preflight, exact replay, or
local runtime supervision. Mixing them into artifact build requests fragments
cache identity and crosses Ato's local consent/state boundary.

### Producer output

The producer response identifies the same `artifact_build_id` and carries the
v0 `materialization_key` compatibility alias. It may report a produced output,
a cache hit, or a rejection, and may carry a build-output layer record, remote
layer reference, provenance, build log reference, and warnings.

Producer output may later become input to an `install_revision_id`. The worker
itself does not allocate `installed_app_id`, `profile_id`, `install_profile_key`,
`install_revision_id`, or `capsule_instance_key`.

### File-backed local worker simulation

The first worker harness is file-backed and local. It validates the producer
request/response boundary without adding a submit API, HTTP service, scheduler,
or trust registry.

The simulator receives a local source fixture out of band, copies it into a
disposable worker workspace, runs only the build phase there, captures the
declared outputs, and exports the same remote CAS mirror layout that ADR-008's
phase materializer already looks up:

```text
<remote_root>/build-output/<materialization_key>/
  layer.json
  blob/
    manifest.json
    payload/
```

The harness is deliberately install-agnostic. It does not allocate
`install_revision_id`, and its producer request still does not receive launch
profiles, secret values, user state, consent records, dynamic session data, or
runtime process identity.

## Consequences

- Good: remote-first public-repo builds and output-first installs can share one
  artifact cache identity without treating a launch envelope as a build key.
- Good: the producer contract is testable as pure data before HTTP APIs,
  schedulers, and actual workers exist.
- Good: existing phase materialization CAS lookup remains usable through the v0
  `materialization_key` alias.
- Bad: artifact producer and phase materialization vocabulary coexist until the
  later install lifecycle owns producer responses.
- Bad: dependency layers, private source input, signing, and trust registry
  remain explicit follow-up work.

## Non-Goals

- remote build submission, HTTP API, worker scheduling, or worker execution,
- install root, revisions, profiles, shortcuts, or Desktop integration,
- dependency-layer relocation for npm, pnpm, Yarn, uv, or native modules,
- signing, provenance trust policy, or registry trust decisions,
- AFFiNE-specific handling.
