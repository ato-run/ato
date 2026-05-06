---
name: ato-issue
description: 'Open a GitHub issue on ato-run/ato distilled from the current session. Use when the user says "issueを作成", "issue化", "open an issue", "file a bug", or otherwise asks to capture a problem/idea uncovered in conversation. Writes the issue in English, attaches the right area:/topic:/platform: labels, and uses the standard ato body template.'
license: MIT
---

# ato-issue

Turn something we just discussed into a well-formed GitHub issue on `ato-run/ato` in one shot — correct repo, correct labels, English body, standard structure. Optimised so the user only has to approve the `gh issue create` permission prompt.

## When to use

Trigger phrases (any language): "issueを作成", "issue化して", "issue立てて", "open an issue", "file a bug", "track this as an issue", "bug report にしておいて".

Do **not** use this skill for:
- Pull requests (use `gh pr create` directly).
- Editing or commenting on an existing issue (use `gh issue edit` / `gh issue comment`).
- Internal-only TODOs that should not be public — confirm with the user first if unsure.

## Preconditions

- Working directory is the `ato` repo (`origin = git@github.com:ato-run/ato.git`). If the remote is something else, stop and ask the user where the issue should go — do not guess.
- `gh` is authenticated (the user has used it earlier in this project; assume yes unless `gh issue create` fails with an auth error).

## Repo & language defaults

- **Repo**: always pass `--repo ato-run/ato` explicitly. Do not rely on the cwd remote — the user runs this from sibling worktrees too.
- **Language**: write the title and body in **English**, even if the conversation was in Japanese. The user has explicitly asked for English issues; do not switch back to Japanese unless they tell you to in this turn.
- **Author**: do not add `Co-Authored-By` trailers anywhere (see `feedback_commit_authorship.md`). Issues only credit the GitHub user who runs `gh`.

## Labels

Source of truth is `gh label list --repo ato-run/ato`. Cache below is current as of 2026-05-06 — re-list if a label you want is missing.

Pick **one type label**:
- `bug` — something is broken or behaves incorrectly
- `enhancement` — new feature or improvement to existing behavior
- `documentation` — docs-only change
- `question` — needs discussion before it's actionable
- `type:rfc` — design discussion / proposal

Pick **one area label** when the surface is obvious:
- `area:ato-cli`
- `area:ato-desktop`
- `area:capsule-core`

Add **topic labels** when relevant (zero or more):
- `topic:dependency-contracts`, `topic:runtime-tools`, `topic:lockfile`, `topic:execution-identity`, `topic:capabilities`, `topic:validation`, `topic:sandbox`

Add **platform labels** only when the issue is platform-specific:
- `platform:windows`

Pass them as a single comma-separated string: `--label "bug,area:ato-desktop"`. Do not invent new labels — if nothing fits, leave the area/topic off rather than create one.

## Title

- Format: `<area>: <imperative summary>` — e.g. `desktop: error screen text is not user-selectable`, `cli: ato ps drops dep-contract snapshot after restart`.
- Use the short area prefix (`cli`, `desktop`, `core`, `lockfile`, `sandbox`, …), lowercase, then a colon.
- Keep under ~72 chars. State the problem, not the fix.

## Body template

Use this skeleton. Drop sections that genuinely have nothing to say (don't pad with "N/A").

```markdown
## Summary

<1–3 sentences. What is wrong / what is being proposed, and who feels it.>

## Current behavior

- <Concrete observation #1, with file paths or commands when known>
- <Observation #2>

## Expected behavior

- <What should happen instead, in user-visible terms>
- <Edge cases we explicitly want preserved>

## Scope

- <Where the fix likely lives — module, crate, component>
- <Out-of-scope items, if the conversation already ruled them out>

## Repro

1. <Step>
2. <Step>
3. <Observed vs expected>
```

For `enhancement` / `type:rfc` issues, replace `Repro` with a `Motivation` or `Alternatives considered` section as appropriate.

## Distilling from the session

Before drafting, scan the conversation for:
- The actual user complaint (quote it loosely in `Summary`).
- Any file paths, commands, error messages, or commit SHAs that came up — those belong in `Current behavior` / `Repro`, not in `Summary`.
- Decisions the user already made ("we don't want to touch X") — encode them in `Scope` so the issue isn't reopened by a future contributor proposing the rejected approach.

If you find yourself inventing details that were never discussed, stop and ask the user instead of fabricating repro steps.

## Command shape

Always use a HEREDOC for the body so newlines and backticks survive the shell:

```bash
gh issue create --repo ato-run/ato \
  --title "<area>: <summary>" \
  --label "<type>,<area>[,<topic>...]" \
  --body "$(cat <<'EOF'
## Summary

…

EOF
)"
```

Never pass `--body` as an inline double-quoted string — backticks and `$` will be interpreted by the shell.

## Permissions

`gh issue create` against `ato-run/ato` is an external write and will hit the harness's permission prompt the first time each session. That is expected — do not try to route around it (no `curl`-to-API workarounds, no asking the user to paste a token). If the prompt is denied, stop and ask the user how they want to proceed.

## After creation

`gh issue create` prints the issue URL on success. Reply to the user with:
- The URL.
- The final title.
- The labels that were applied.

Keep the reply to ~3 lines unless the user asked for a writeup. Do not paste the full body back — they can click the link.

## Worked example (this session)

Conversation: user reported that the desktop error screen text could not be selected.

```bash
gh issue create --repo ato-run/ato \
  --title "desktop: error screen text is not user-selectable" \
  --label "bug,area:ato-desktop" \
  --body "$(cat <<'EOF'
## Summary

In the desktop app, the text shown on error screens cannot be selected. Users cannot copy the error message to search for it, paste it into a bug report, or share it with support.

## Current behavior

- Error screen text behaves as if `user-select: none` (or an equivalent setting) is applied, so neither mouse drag nor keyboard selection works.
- The only way to capture an error message is to retype it manually.

## Expected behavior

- All textual content on error screens (message, details, stack trace, IDs) should be selectable and copyable with the standard OS shortcuts.
- Non-text UI affordances (buttons, icons) keep their current non-selectable behavior.

## Scope

- Audit the error screen component(s) in the desktop app and scope down the `user-select: none` rule.
- Do not regress text selection elsewhere; do not make toolbars/buttons selectable.

## Repro

1. Trigger any error screen in the desktop app.
2. Try to select the error text with the mouse or `Cmd/Ctrl+A`.
3. Observe that nothing is selected and the text cannot be copied.
EOF
)"
```

Result: https://github.com/ato-run/ato/issues/38, labels `bug`, `area:ato-desktop`.
