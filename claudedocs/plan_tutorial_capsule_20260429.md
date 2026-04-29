---
title: "ato Interactive Tutorial — Implementation Plan"
date: 2026-04-29
author: "@egamikohsuke (per design v0.1)"
status: proposal
related:
  - samples/tutorial/docs/DESIGN.md (authoritative design)
  - crates/ato-desktop/src/ui/modals/mod.rs (modal infra reuse)
  - crates/ato-desktop/src/ui/chrome/mod.rs (omnibar)
  - crates/capsule-wire/src/handle.rs (URL parsing)
  - crates/ato-cli/src/cli/commands/inspect.rs (manifest preview)
---

# ato Interactive Tutorial — Implementation Plan

Maps the design at `samples/tutorial/docs/DESIGN.md` (v0.1) to a sequenced
build plan. **Sub-repo `samples/tutorial/` is already scaffolded and
committed locally** with the design doc, READMEs, and 5 capsule stubs
matching the design's manifest values. Wiring the sub-repo as a real git
submodule in the parent is a one-liner once a remote URL is chosen.

## 0. Current state vs design (recon)

Verified against `main` at commit `06820ce`:

| Area | Shipped | Gap vs design |
|---|---|---|
| Omnibar URL entry | `ato-desktop/src/ui/chrome/mod.rs:19-48` accepts capsule URLs and dispatches `NavigateToUrl` via `state/mod.rs:1799` | `ato.run/s/<name>@<rev>` share-URL form is not yet recognized by `normalize_capsule_handle` |
| Modal infra | `ato-desktop/src/ui/modals/mod.rs` exists; `config_form` modal already lives there | No confirmation-modal variant yet — pattern is documented and reusable |
| Manifest metadata fetch | `ato inspect` emits structured JSON (`InspectRequirementsResult` in `crates/ato-cli/src/cli/commands/inspect.rs:37`) | Desktop does not yet call `ato inspect` to populate a preview |
| `ato run <url>` resolution | Accepts `<publisher>/<slug>[@<version>]`, `github.com/...`, local paths | `ato.run/s/...` share URLs not natively resolved |
| `ato encap` / `decap` / `publish` | **None ship** — `ls crates/ato-cli/src/cli/commands/` confirms | Hard blocker for design Steps 4, 5, 6, 7 |
| `state.workspace` (persistent FS) | Schema validated (`StateKind::Filesystem` + `StateDurability::Persistent`); `--state-bindings` flag wired in `RunArgs` | Auto-mount into Desktop sessions incomplete; no `attach = "auto"` codepath end-to-end yet |

**Implication:** the Try-flow (Steps 1–3) is achievable with mostly UX
work in Desktop + the URL parser update. The Share-flow (Steps 5–7) and
Step 4 are blocked on CLI commands that don't exist yet.

## 1. Phasing

The phases are ordered so each one ships value independently. Phases A–C
are unblocked today. Phases D–F depend on CLI work outside the tutorial
slice.

```
A: scaffold      ← done (initial commit in sub-repo)
B: tutorial-app  ← unblocked
C: try-flow E2E  ← unblocked (needs Desktop modal + URL form work)
─────────── handoff barrier (encap/decap/publish CLI work) ─────────
D: step 4        ← needs `ato encap` / `ato decap`
E: share wizard  ← needs `ato publish` + manifest scaffolder
F: polish        ← i18n, animation, metrics
```

### Phase A — Scaffold (DONE)

- [x] Sub-repo `samples/tutorial/` initialised (`git init`, branch `main`)
- [x] `docs/DESIGN.md` copied verbatim from design v0.1
- [x] 5 capsule directories with `capsule.toml` per design values
- [x] Minimal placeholder source files (Deno servers, Python hello, scaffold)
- [x] Initial commit `45034cb` on sub-repo
- [ ] **Submodule wire-up** (deferred until remote URL is chosen)
  ```bash
  # In parent repo, once remote exists:
  git submodule add <remote-url> samples/tutorial
  git commit -m "samples: register tutorial submodule"
  ```

### Phase B — `tutorial-app` UI capsule

Goal: a working UI shell that runs locally (`ato run ./apps/tutorial-app`)
and renders the seven step cards. No Desktop integration yet — the user
still uses CLI commands directly.

Tasks:

1. **Shell HTML/CSS** — single page, seven step cards, no SPA framework
   (per design §6: Deno + raw HTML/CSS).
2. **WebSocket-backed log streamer** — when the page asks "run step N",
   the server `Deno.Command`s `ato run <step-url>` and pipes stdout/stderr
   over a WebSocket to the page.
3. **Persistent progress** — `tutorial-progress.json` in `Deno.cwd()`,
   read on boot, written on each step completion. Schema:
   ```json
   { "version": 1, "completed": ["step1"], "currentStep": "step2" }
   ```
   File lands inside the `state.workspace` FS once Phase D of the plan
   wires `attach = "auto"`. Until then the tutorial works fine — the
   file lives in cwd and is picked up on the next run.
4. **Local-mode URL rewrite** — when `ATO_TUTORIAL_LOCAL=1`, the Run
   handler maps `ato.run/s/tutorial-stepN@r1` → `./apps/tutorial-stepN`.
   Lets the tutorial be developed without a published version.
5. **Validation** — `ato inspect preview ./apps/tutorial-app` produces a
   non-error JSON; `ato run ./apps/tutorial-app` serves on `:3000`.

Files to write inside `samples/tutorial/apps/tutorial-app/`:
- `src/index.html`
- `src/styles.css`
- `src/main.ts` (replace placeholder with real server)
- `src/run_step.ts` (subprocess wrapper)
- `src/progress.ts` (JSON I/O)
- `src/url_resolver.ts` (local-mode rewrite)

Phase B has zero changes to the parent ato repo. It's pure tutorial
content work.

### Phase C — Try-flow E2E in ato-desktop

Goal: paste `ato.run/s/tutorial-step1@r1` into the omnibar → orange
confirmation modal → step 1 runs in a pane. No changes needed to step1
content; this phase is entirely in `crates/ato-desktop/` + a small
addition to `crates/capsule-wire/`.

#### C.1 — Recognize share URLs

`crates/capsule-wire/src/handle.rs` — extend `normalize_capsule_handle`
to accept `ato.run/s/<slug>[@<rev>]` and emit the canonical
`capsule://ato.run/<publisher>/<slug>[@<rev>]` form. Tests follow the
same pattern as the existing `acme/chat` test cases.

Ambiguity to resolve in design: the design's URL says
`ato.run/s/tutorial@r1`, but the canonical handle includes a publisher
namespace (`ato-run/tutorial`). Two compatible options:

- **Option 1**: `ato.run/s/<slug>` resolves to the `ato-run/<slug>`
  registry entry (well-known publisher prefix).
- **Option 2**: Share URLs always carry the publisher: `ato.run/s/ato-run/tutorial@r1`.

Recommend **Option 1** — fewer characters in marketing material,
matches the design's example URLs. Encode the well-known-publisher
mapping as a const in `capsule-wire`.

#### C.2 — Capsule confirmation modal

New module `crates/ato-desktop/src/ui/modals/capsule_confirm.rs`,
following the `config_form` pattern. State trigger lives on `AppState`
as `pending_capsule_confirm: Option<CapsuleConfirmRequest>`.

Modal payload (struct):
```rust
struct CapsuleConfirmRequest {
    canonical_url: String,        // ato.run/s/tutorial@r1
    publisher: String,
    publisher_verified: bool,     // signature check from registry
    capsule_type: String,         // "app" | "job" | "tool"
    runtime_label: String,        // "web/deno 1.41.0"
    network_summary: String,      // "egress なし" / domains
    sandbox_tier: String,         // "Nacelle (Tier1)"
}
```

Populated by calling `ato inspect <url> --json` in the background as
soon as the omnibar URL parses successfully. Until inspect resolves,
the modal can show a skeleton with the URL only.

Visual: orange `#E8500A` background, `#FF6B2B` accent, green verified
badge, primary button "実行する →", secondary "キャンセル". Match the
existing `gpui::theme` color tokens; if missing tokens, add them under
`crates/ato-desktop/src/ui/theme.rs`.

#### C.3 — Skip-flag settings

New section in the Settings tab (`crates/ato-desktop/src/state/mod.rs`
already has `SettingsTab`). Two checkboxes per design §2.3:

- `skip_confirm_for_trusted_publishers: bool`
- `skip_all_confirms: bool` (red warning text — "危険操作")

Persisted alongside other Desktop settings (existing serde Settings
struct).

#### C.4 — Inspect-driven preview pre-fetch

When the omnibar resolves a URL but the user has not yet confirmed,
spawn `ato inspect <url> --json` (background `tokio::process::Command`).
Cache the result keyed by canonical URL for 60 s. The modal blocks on
this future the first time, then pulls from cache.

#### Risk callout — god-file pressure

Phase C lands ~300–400 LOC of new modal + state machinery. From the
prior `/readable-code` audit, `state/mod.rs` is already 5,079 LOC —
the largest "Tier B" file. Adding `CapsuleConfirmRequest` + the
settings flags + the cache without splitting `state/mod.rs` first will
make the existing problem worse.

**Recommendation:** do `state/mod.rs` decomposition (audit Top-3 #1)
**before** Phase C lands, or scope the Phase C state additions into
`state/capsule_confirm.rs` to avoid the existing god-file. Either way,
land the new code in its own submodule under `state/` not the root.

### Phase D — Step 4 (encap → URL → decap)

**Blocked on `ato encap` and `ato decap` not existing.** These are
separate workstreams. Tutorial Step 4 is just the user-facing
description; the actual mechanism needs:

1. `ato encap <path>` — packages a directory into a deterministic
   blob, signs, returns a `capsule://` URL. (Likely `crates/ato-cli/src/cli/commands/encap.rs`.)
2. `ato decap <url>` — inverse: fetches and unpacks for inspection.

The tutorial UI's "Run Step 4" button shells out to these. Until they
exist, the step card stays in a "coming soon" state with a link to the
parent issue. **Do not block Phases B/C on this.**

### Phase E — Share-flow (Steps 5–7)

**Blocked on `ato publish` not existing.** Step 5–7 require:

1. **Step 5: capsule.toml wizard.** Lives in the tutorial UI. Reads
   user answers, writes a manifest. No CLI dependency.
2. **Step 6: `ato inspect preview` integration.** Already exists; the
   wizard runs it and renders the JSON output as a structured panel.
3. **Step 7: `ato publish`.** Hard blocker. Needs OIDC sign + Store
   upload paths in ato-cli that don't exist today.

Step 5 alone can ship as a partial Phase E once Phase B/C are green —
it gives publisher candidates the manifest authoring experience even
without the publish endpoint.

### Phase F — Polish & deferred decisions

From design §7 ("未決事項"):

- **i18n** — recommend EN-only at MVP, JA layer added if metrics show
  demand. Adding i18n upfront doubles content cost.
- **Modal animation** — defer to a UX pass after Phase C is shipped
  and we have screen recordings of real users.
- **Metrics** — Step-completion telemetry needs a privacy review
  before any data leaves the user's machine. Defer to a separate RFC.

## 2. Cross-cutting risks

| Risk | Mitigation |
|---|---|
| Adding 300+ LOC to `state/mod.rs` (already 5,079 LOC) | Scope new state into `state/capsule_confirm.rs` submodule from day one; or land the audit's Top-3 #1 split first |
| `ato inspect` taking >1 s on cold cache makes the modal feel sluggish | Phase C.4 prefetch + 60 s memoization; skeleton modal during the first inspect call |
| Tutorial capsules get out of sync with `schema_version` bumps | The sub-repo at `samples/tutorial/` becomes part of the parent's CI matrix once the submodule is wired — `cargo test --test cli_tests` should add a "validate every sub-repo capsule.toml" step |
| Submodule wire-up forgotten on fresh clones | Document in parent `README.md` and add `git submodule update --init` to the bootstrap script |
| Step 4 UX shows "coming soon" too long, breaks the narrative | Acceptable for v0.1; the design explicitly puts Try-flow ahead of Share-flow because Share is the longer-tail audience |

## 3. Concrete next moves (smallest valuable PRs)

1. **PR-1 (parent repo)** — register the submodule once a remote exists:
   ```bash
   git submodule add <remote-url> samples/tutorial
   ```
2. **PR-2 (tutorial sub-repo)** — Phase B.1–B.3: real `tutorial-app`
   shell + WebSocket log streamer + progress persistence.
3. **PR-3 (parent repo, capsule-wire)** — Phase C.1: extend
   `normalize_capsule_handle` to accept `ato.run/s/<slug>` form. Add
   tests. ~80 LOC including tests.
4. **PR-4 (parent repo, ato-desktop)** — Phase C.2: new
   `capsule_confirm` modal module + state field. ~300 LOC.
5. **PR-5 (parent repo, ato-desktop)** — Phase C.3 + C.4: skip-flag
   settings + inspect prefetch.

Each of PR-3 / PR-4 / PR-5 is independently shippable and behind feature
detection (the modal only triggers if the URL resolves, and skip-flags
are off by default per design §2.2). PR-3 is the smallest and safest
starting point.

## 4. Out of scope for this plan

- The `ato encap` / `decap` / `publish` CLI commands (separate RFCs).
- Persistent-workspace auto-mount completion (separate Phase 10 work
  per `docs/TODO.md`).
- Tutorial content in languages other than the design's example text.
- Marketing site copy and the `ato.run` registry entries (handled by
  whoever runs the publisher account once the publish CLI ships).

## 5. Submodule wire-up — exact commands

When the team chooses a remote (e.g. `github.com/ato-run/tutorial`),
run from the parent repo root:

```bash
# Push the local sub-repo to the remote first.
cd samples/tutorial
git remote add origin git@github.com:ato-run/tutorial.git
git push -u origin main
cd ../..

# Register as a submodule. The directory already exists with the right
# contents, so this writes .gitmodules and pins the commit.
git submodule add git@github.com:ato-run/tutorial.git samples/tutorial
git commit -m "samples: register tutorial submodule"
```

After this, fresh clones run `git submodule update --init` to fetch.
