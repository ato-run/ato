# Desktop sample-recipes AODD — pre-fix Phase 1 baseline

**Branch:** `test/desktop-sample-recipes-aodd`
**Base:** `dev` @ `1758a981` (includes PR #251 + #252; preflight fix from PR #253 NOT yet merged)
**Date:** 2026-05-24
**Companion PR:** #253 (preflight routing finding for Test Set A; this AODD verifies the same
finding still holds and extends scope to Blinko / AFFiNE / Dify)

## Why this AODD stopped at Phase 1

The brief's stop condition is explicit:

> ここで失敗した場合、Desktop AODD に進まない。原因は Desktop ではなく CLI preflight routing。

Phase 1 fails for **all 8 targets** for two distinct reasons. Per the brief, Desktop drive
(Phase 2), negative-test through Desktop (Phase 3), and session/process verification (Phase 4)
were not attempted because the prerequisite CLI routing layer is still broken.

## Phase 1 result matrix

| Target | `app resolve <alias>` | `internal preflight <alias>` | `internal preflight <github>` | Class |
|---|---|---|---|---|
| memos | ✅ kind=sample_recipe | ❌ E999 registry-handles | ❌ E999 manifest-not-exist | A: preflight gap |
| uptime-kuma | ✅ kind=sample_recipe | ❌ E999 registry-handles | ❌ E999 manifest-not-exist | A: preflight gap |
| n8n | ✅ kind=sample_recipe | ❌ E999 registry-handles | ❌ E999 manifest-not-exist | A: preflight gap |
| open-webui | ✅ kind=sample_recipe | ❌ E999 registry-handles | ❌ E999 manifest-not-exist | A: preflight gap |
| excalidraw | ✅ kind=sample_recipe | ❌ E999 registry-handles | ❌ E999 manifest-not-exist | A: preflight gap |
| **blinko** | ❌ E999 unsupported handle | ❌ E999 registry-handles | ❌ E999 manifest-not-exist | B: no recipe + no catalog + preflight gap |
| **affine** | ❌ E999 unsupported handle | ❌ E999 registry-handles | ❌ E999 manifest-not-exist | B: no recipe + no catalog + preflight gap |
| **dify** | ❌ E999 unsupported handle | ❌ E999 registry-handles | ❌ E999 manifest-not-exist | C: recipe exists, missing catalog entry + preflight gap |

**Class A (5 apps):** PR #252 wiring works at `app resolve`; the Desktop's `internal preflight`
call site doesn't consult the sample recipe catalog. Identical to PR #253's finding.

**Class B (Blinko, AFFiNE):** No `samples/recipes/<app>/capsule.toml` in the repo. Even with
the preflight fix, these can't reach session-created until someone authors the recipe AND
registers it in `SAMPLE_RECIPE_CATALOG`.

**Class C (Dify):** Recipe IS authored at `samples/recipes/dify/capsule.toml` (v1.14.2, full
6-service compose), but Dify is missing from `SAMPLE_RECIPE_CATALOG`. Single-line catalog
addition would move it to Class A.

## What changed since PR #253

Nothing in `dev`. The five Test Set A regression rows are byte-identical to PR #253's evidence
(same error codes, same paths under fresh `ATO_HOME`). This AODD's value is:

1. **Regression confirmation** — PR #253's finding still holds after a clean rerun.
2. **Scope expansion** — Blinko / AFFiNE / Dify reveal a second layer of work even beyond
   PR #253's preflight fix.
3. **Negative-path baseline** — `app resolve` correctly surfaces upstream 404
   `repo_not_found`, but `internal preflight` collapses every failure to the generic
   "manifest path does not exist". The Desktop will inherit whichever message the underlying
   call returns, so the preflight fix should also propagate the upstream cause.

## What the next slice must do, in order

1. **Land PR #253's preflight routing fix** (mirror `app_control::resolve::normalize_handle`'s
   sample-recipes early-return inside `ato internal preflight`'s handle classifier).
2. **Make the launch-fallback path honest** — replace the silent stall after preflight failure
   with either a visible Control Bar error or a genuine completion from the resolver snapshot.
3. **Author missing recipes** for Blinko + AFFiNE (Class B → Class A).
4. **Register Dify** in `SAMPLE_RECIPE_CATALOG` (Class C → Class A, one-line change).
5. **Propagate upstream cause in preflight errors** so the negative-test failure reads
   `repo_not_found` instead of `manifest path does not exist`.
6. **Re-run this AODD** with the same 8 targets + the negative case, and measure session-created
   reach rate. Until then, Phase 2/3/4 cannot run.

## Final report (per brief format)

```text
AODD complete.

Headline:
  Desktop sample recipe routing: BLOCKED (Phase 1 fails; Desktop AODD did not run)

Reach rate:
  Existing Test Set A: 0/5 session-created (Phase 1 fails — preflight regression confirmed)
  New apps:
    Blinko: blocked (Class B — no recipe, no catalog, preflight gap)
    AFFiNE: blocked (Class B — no recipe, no catalog, preflight gap)
    Dify:   blocked (Class C — recipe authored but not catalogued; preflight gap)

Key findings:
  - PR #253's preflight gap is fully reproducible and unchanged in dev@1758a981.
  - Dify has an authored recipe (samples/recipes/dify/) that's invisible to the
    resolver because nobody registered it in SAMPLE_RECIPE_CATALOG. One-line fix
    to unblock once the preflight routing also lands.
  - Blinko + AFFiNE have no recipes in the repo — they need authoring before any
    routing AODD can reach them.
  - Negative-test CLI behaviour differs by call site: `app resolve` surfaces the
    upstream 404 / repo_not_found, but `internal preflight` collapses it to a
    generic "manifest path does not exist". Even after the preflight fix lands,
    the error-cause propagation may need a follow-up so the Desktop's negative
    path shows the actionable cause instead of the generic path message.

Regression check:
  - internal preflight sample recipe alias: fail (memos/uptime-kuma/n8n/open-webui/excalidraw all E999)
  - capsule://github sample recipe mapping: fail (all 5 fall to GitHub-clone path inference)
  - local path precedence: not_tested (no local capsule.toml in scope this run)
  - silent fallback removed: not_tested (Desktop drive skipped per stop condition)

Receipts:
  - .tmp/aodd-receipts/desktop-sample-recipes/blinko.yaml
  - .tmp/aodd-receipts/desktop-sample-recipes/affine.yaml
  - .tmp/aodd-receipts/desktop-sample-recipes/dify.yaml
  - .tmp/aodd-receipts/desktop-sample-recipes/negative-missing-sample.yaml
  (Test Set A receipts already shipped in PR #253 under .tmp/aodd-receipts/launch-routing/)

Consolidated doc:
  - docs/recipes/desktop-sample-recipes-aodd.md

Next slice:
  1. Merge PR #253 (or land equivalent preflight routing fix).
  2. Register Dify in SAMPLE_RECIPE_CATALOG.
  3. Author + register Blinko and AFFiNE recipes.
  4. Re-run this AODD with Desktop drive enabled; expected outcome is Class A
     for all 8 targets at the resolver layer, and session-created OR visible
     actionable error at the Desktop layer for each.
```

## Environment

```text
Worktree:  .worktrees/desktop-sample-recipes-aodd   (test/desktop-sample-recipes-aodd)
Binaries:  reused from .worktrees/launch-routing-aodd/target/release/  (ato 0.5.2, nacelle 0.5.2)
ATO_HOME:  mktemp -d -t ato-sample-recipes-aodd-XXXXXX  (hermetic, fresh per run)
podman:    applehv machine running; not exercised this run (no session-start attempted)
```
