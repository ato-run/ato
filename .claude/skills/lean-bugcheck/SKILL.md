---
name: lean-bugcheck
description: Find real bugs in ato by modelling small security-critical decision logic in Lean 4, proving the intended property, and turning failed proofs into kernel-checked counterexamples plus Rust reproductions. Also refreshes the dead/unused-code inventory. Use when asked for a "Lean bug check", formal verification, proof-based bug hunting, or an unused-code sweep of this repo.
---

# Lean bug check (proof-failure-driven bug hunting)

Everything lives in `formal/lean-bugcheck/`:

| File | Role |
|---|---|
| `AtoBugcheck.lean` | Lean 4 models (core Lean only, no Mathlib, no `sorry`). One `namespace` per Rust target. |
| `repro/` | Rust reproductions of every `*_BUG` theorem against the real source. |
| `dead-code/scan.py` | Unused-code heuristics (never-referenced / test-only `pub` items, orphan modules, maybe-unused deps, `allow(dead_code)` census, rustc warnings). |
| `run.sh` | Runs Lean check → repros → (optionally) dead-code scan in one go. |
| `REPORT.md` | Findings so far (Japanese). Update it, don't fork it. |

## 0. Setup (once per machine)

```bash
command -v lean || curl -sSf https://raw.githubusercontent.com/leanprover/elan/master/elan-init.sh | sh -s -- -y --default-toolchain stable
```

`run.sh` uses `$LEAN` or `~/.elan/bin/lean`, exits on a failed check, and uses a temporary directory under the worktree `.tmp/`. If the pinned Rust toolchain cannot be downloaded, run with `RUSTUP_TOOLCHAIN=stable`.

## 1. Re-verify the existing results first (cheap)

```bash
formal/lean-bugcheck/run.sh                 # ~1 min
RUN_CHECK=1 formal/lean-bugcheck/run.sh     # + cargo check + dead-code scan (slow, run in background)
```

Existing repro programs now assert the fixed behavior. If a regression reappears they fail.
When fixing another finding, replace its `*_BUG` theorem with a positive property and update `REPORT.md`.

## 2. Pick targets — minimise tokens, plan before reading

Do the selection with one shell command, not by browsing:

```bash
grep -rlE --include=*.rs 'fn (is_allowed|authorize|check_[a-z_]+|permits?|allow|matches_[a-z]+|normalize_[a-z]+|validate_[a-z]+|sanitize|consume|parse_(authority|range|cidr|port))' . | grep -v /target/ | xargs wc -l | sort -n | head -40
```

Prefer files that are **small, pure, and security-relevant** (authorization, path containment, egress/CIDR,
budgets/limits, replay sets, manifest validation). Skip already-modelled namespaces (see `REPORT.md`).
Read only the non-test part of each file in one batch:

```bash
for f in A.rs B.rs; do echo "== $f"; awk '/^#\[cfg\(test\)\]/{exit} {print}' "$f" | grep -vE '^\s*(//|$)'; done
```

Then grep the call sites of the chosen functions in one command to know whether a bug is reachable.

## 3. Model in Lean

Conventions in `AtoBugcheck.lean`:

- Header comment per section: `## N. <path> :: <function>`.
- Model only the decision logic. Abstract the filesystem / time / network as function parameters
  (e.g. `ex : Path → Bool` for `exists()`), numbers as `Nat`, and add an explicit `...U64` variant when
  wrap-around matters.
- State the property the code is *supposed* to have (from docs, names, tests) as `spec`.
- Try to prove it. If the proof fails, find a concrete witness and commit it as
  `theorem <name>_BUG : ∃ ..., code = true ∧ spec = false := ⟨witness, by decide⟩`.
- Where cheap, also prove the fix correct (`fixed_sound`) so the suggested patch is backed by a proof.
- Properties that hold are kept too (they document what was checked and rule out suspicions).
- Unreachable branches found along the way become `theorem ..._dead` and go into the unused-code section.

Check: `~/.elan/bin/lean formal/lean-bugcheck/AtoBugcheck.lean` (exit 0, `grep -c sorry` = 0).
Useful tactics: `decide` for concrete witnesses, `simp [defs] at *; omega` for arithmetic, `induction` for
absorbing states, `cases`/`by_cases` for enums.

## 4. Reproduce every `*_BUG` against the real code

A Lean counterexample is only a lead — the model may be unfaithful. Reproduce before reporting:

- Self-contained module: `#[path = "../../../<file>.rs"] mod m;` in `repro/<name>.rs`, `fn main()` that
  asserts the bad behaviour (see `path_widening.rs`).
- Extractable item: copy it with `awk` into `$OUT` and `include!` it (see `budget_wrap.rs`).
- Needs crate internals: add a permanent regression test in the owning module and run
  `cargo test -p <crate> --lib <name>`. Do not mutate source temporarily inside `run.sh`.

Add the new step to `run.sh`.

## 5. Report

Update `REPORT.md`: table row (location, behaviour, Lean theorem, repro status, real-world impact and
reachability), "properties proven", unused-code list, limitations. Be explicit about what was **not**
modelled or reproduced. Do not file issues or PRs unless asked.

## Limits to state every time

Hand-written models (faithfulness rests on reading); symlinks/concurrency usually abstracted; no mutation
testing of specs unless done; `scan.py` is identifier-based, so macro-generated uses and external API can
show up as false positives.
