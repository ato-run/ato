---
title: "Static Web delivery detection"
status: draft
date: "2026-08-10"
owner: "snapshot-builder, capsule, ato-api"
---

# Static Web delivery detection

## Decision

Source ecosystem and delivery are independent facts. A Node toolchain may
produce a static Web output, so detectors must not classify a project as either
`Node` or `Static`. They collect source evidence, then propose a typed static
output candidate only when its output is causally tied to a supported build.

The v1 Program Intent and `capsule.toml` carry that candidate as
`static_web_output` / `[outputs.static_web]`. The output contract contains the
output root, entry path, SPA fallback policy, and allowed connect sources.

## Initial detector policy

1. Explicit `[outputs.static_web]` remains authoritative.
2. A root `index.html` without `package.json` is static with no build step.
3. A root `index.html` plus known static builds is static:
   - Vite `vite build` → `dist/`
   - Create React App `react-scripts build` → `build/`
   - Astro `astro build` → `dist/`
   - Eleventy `eleventy` → `_site/`
4. A Node server command (`start`/`dev`, excluding Vite development or preview)
   refuses static inference even when `dist/` exists.
5. A Vite configuration that declares a custom `outDir` requires manual setup
   until that setting is parsed into a verified output contract.

The detector never chooses Static merely because a `dist/` directory exists.
The static producer validates the clean-build output tree before materializing
or publishing it.

## Non-goals

This change does not add a new runner, a static producer dispatch path, or
multi-surface execution. It only connects safe source evidence to the typed
authoring output contract.
