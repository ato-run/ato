# Contributing to ato-samples

This repo is **opinionated and curated**. Raw count is not a goal; every sample must earn its place.

## Adding a sample

1. Open a discussion first. Describe the capability or user problem the sample proves, which tier it fits, and why an existing sample doesn't already cover it.
2. Author the sample against the file contract below.
3. CI must be green on every OS declared in `health.toml`.
4. A maintainer reviews for: tier fit, minimality (one concept per sample), stability (no flaky external deps), and English-only surface.

## File contract (mandatory)

Every sample directory must contain:

```
<sample-slug>/
  capsule.toml              # ato manifest
  health.toml               # CI spec — see tools/schemas/health.schema.json
  README.md                 # with YAML front-matter (see template below)
  EXPECTED.md               # success (Tier 00-02) or expected-failure (Tier 03) output
  <runtime lockfile(s)>     # pnpm-lock.yaml / uv.lock / Cargo.lock, whichever applies
  capsule.lock.json         # committed
  ato.lock.json             # committed
```

### README.md front-matter

```markdown
---
title: <human title>
tier: 02-apps
runtime: source/node
difficulty: intermediate
time: ~5min
tags: [byok, chat, network-policy]
requires: [OPENROUTER_API_KEY]
capability: [run, env-preflight, network-policy]
min_ato_version: 0.5.0
verified_on: 2026-04-20
ato_version: 0.5.0
---

## Why this sample exists

...
```

## Backwards-compatibility rules

- `min_ato_version` is **immutable** once set on a sample in a release. A git-history lint rejects PRs that change it.
- From `ato` v0.5 onward, every sample must run unchanged on every later `ato` release. Breakage is a release blocker.
- When `ato` must make a breaking change, the sample does not change — `ato migrate` handles it. If migration is not possible, the sample is retired in a dedicated release note.

## Language policy

All sample code, READMEs, comments, and UI strings are **English-only**. The parent workspace is migrating `ato-cli`, `ato-desktop`, and `desky` to EN as well — see `TODO.md` in the parent repo.

## Flakiness policy

Retries are **not** a first-line fix. A flaky sample must:

1. Be quarantined (`[flaky].quarantined = true` in `health.toml`) with a linked GitHub issue.
2. Land a real fix (usually: mock the upstream, pin the dep, or drop the sample) within one release cycle.

Retries are audited in the weekly flaky report.

## Rejecting samples

Samples are rejected (politely) when they:

- Duplicate an existing sample's teaching goal.
- Require a paid API we cannot mock.
- Demonstrate a pattern that is not idiomatic `ato`.
- Cannot fit in ≤500 LOC excluding lockfiles.
- Fail on any target OS declared by their tier.

Rejected community samples are welcome in [SHOWCASE.md](SHOWCASE.md) as links to external repos.
