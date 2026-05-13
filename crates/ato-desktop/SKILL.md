---
name: ato-desktop-gpui-html-workflow
description: "How an AI agent converts mockup HTML into ato-desktop's GPUI Rust components. Use the external `gpui-html` CLI to lower HTML to a Rust builder skeleton; integrate it into production components by hand — do NOT add an HTML parser to ato-desktop."
audience: agents working on ato-desktop UI
---

# Mockup HTML → ato-desktop component workflow

When the user hands you HTML/CSS for a desktop UI element (sidebar tab,
modal, chrome bar, …) and asks you to ship it as a GPUI component
inside `crates/ato-desktop`, **do not parse the HTML yourself, and do
not add a parser dependency to this crate**. Use the external
[`gpui-html`](https://github.com/ato-run/gpui-html) compiler to lower
the HTML to a GPUI builder Rust skeleton, then integrate that skeleton
into a hand-written production component.

## Why the boundary exists

`gpui-html-core` owns "HTML → GPUI builder Rust". `ato-desktop` owns
"GPUI builder Rust → production component wired to live `AppState`,
theme, assets, and input handlers". The two surfaces evolve at
different rates:

- HTML/CSS lowering rules change with gpui-html's spec (utility classes,
  theme tokens, manifest support).
- Component integration changes with ato-desktop's state model
  (`AppState`, `Workspace`, `Pane`, action enum, `BridgeProxy`, …).

If ato-desktop took a `gpui-html-core` dependency directly, every
spec edit on the parser side would bounce the desktop crate's build,
and agents would be tempted to call the lowering pipeline at runtime
("compile mockup at startup") — which is the wrong architecture.
gpui-html is a **build-time / design-time** tool, not a runtime
dependency.

## Responsibility split

### `gpui-html` is responsible for

- Parsing full HTML documents (DOCTYPE, `<html>`, `<head>`, `<body>`,
  `<script>`, `<style>` are accepted and the boilerplate is stripped).
- Lowering `<style>` rules to GPUI builder method calls.
- Resolving utility classes (`flex`, `gap-2`, `bg-accent`, …).
- Resolving theme tokens and custom-scale sizing via an optional
  TOML theme manifest.
- Emitting a Rust source fragment of `gpui::div().flex().child(…)`
  style builder chains.
- Reporting structured diagnostics with line/column spans.

The generated output is the **truth source for visual structure**.

### `ato-desktop` is responsible for

- Reading the generated Rust and porting the relevant chains into a
  production component under `src/ui/`.
- Plumbing the component into the existing `Theme` adapter
  (`src/ui/theme.rs`) — generated code references `theme.<token>`,
  which must resolve against the real `Theme` struct fields.
- Plumbing asset loading (icons, fonts) through ato-desktop's asset
  pipeline. Generated `img(src=…)` references are placeholders.
- Wiring stateful behavior: sidebar collapse, tab close, active state,
  hover/group transitions, drag-and-drop reorder, click handlers,
  keyboard shortcuts. None of this exists in the HTML mockup or in
  the generated code.
- Connecting actions to the dispatcher (`crate::app::CloseTask`,
  `SelectTask`, `MoveTask`, `ShowSettings`, …).
- Keeping the production component idiomatic — splitting subcomponents,
  introducing `Stateful`/`InteractiveElement` where needed, replacing
  generated `div().child(…)` chains with real `Image`/`Icon`/`Stateful`
  primitives where mockup placeholders don't translate 1:1.

### What `ato-desktop` MUST NOT do

- **Do not add `gpui-html-core` as a dependency** in `Cargo.toml`.
- **Do not build an HTML parser or class-string interpreter** anywhere
  in `src/`. If you see yourself reaching for `regex`/`scraper`/a
  hand-rolled tokenizer to consume HTML, stop and run the gpui-html
  CLI instead.
- **Do not blindly paste generated Rust into production**. The output
  is a structural skeleton; production code needs typing, state hooks,
  and idiomatic GPUI patterns the lowering pipeline can't infer.
- **Do not commit large generated `.rs` files** to the crate. The
  workflow uses `.tmp/` (gitignored) for intermediate artifacts; only
  the hand-integrated component code lands in `src/`.

## Workflow

### 0. Install / locate the CLI

`gpui-html` lives in a separate repository. Install it once per dev
machine:

```bash
cargo install --git https://github.com/ato-run/gpui-html gpui-html
```

After install, `gpui-html --version` should print a `0.x` version.

If the CLI isn't installed and the user expects you to install it for
them, ask first — `cargo install` writes to `~/.cargo/bin/` and is a
durable side effect.

### 1. Save the mockup HTML

Use a gitignored scratch area so intermediate artifacts don't pollute
the crate. The crate's `.gitignore` already excludes `.tmp/`; if it
doesn't, add the entry.

```bash
mkdir -p .tmp/gpui-html
cat > .tmp/gpui-html/mockup.html <<'EOF'
<!-- paste the mockup HTML here -->
EOF
```

If the user pasted HTML directly in chat, write it to the file with
the `Write` tool rather than `cat`-piping a heredoc — heredocs choke
on backtick-fenced content inside the HTML.

### 2. Author a theme manifest

`gpui-html` needs to know the host's color tokens to validate
`bg-<token>` / `text-<token>` / `border-<token>` references and to
lower `bg-<token>/<alpha>` to packed RGBA literals. The manifest is
TOML and lives next to the mockup:

```bash
cat > .tmp/gpui-html/theme.toml <<'EOF'
[colors]
# Mirror the fields in src/ui/theme.rs that the mockup references.
# Hex values must match the live Theme so the generated literals match
# the runtime colors.
base    = "#09090b"
surface = "#18181b"
accent  = "#6366f1"
"accent-foreground" = "#ffffff"

[max-width]
# Custom Tailwind scale entries (e.g. max-w-128) — mirror the mockup's
# Tailwind config.
"128" = "32rem"
EOF
```

`src/ui/theme.rs` is the authoritative list of theme fields. If the
mockup references a token that isn't in `Theme`, decide explicitly:
add it to `Theme` (production change) or rewrite the mockup to use an
existing token (mockup change). Do not silently invent fields in the
manifest just to make the compile succeed.

### 3. Lower the HTML

```bash
gpui-html compile .tmp/gpui-html/mockup.html \
  --manifest .tmp/gpui-html/theme.toml \
  -o .tmp/gpui-html/mockup.rs
```

Use `check` if you only need diagnostics (no output file):

```bash
gpui-html check .tmp/gpui-html/mockup.html \
  --manifest .tmp/gpui-html/theme.toml
```

JSON diagnostics (one per stderr line) for editor / CI parsing:

```bash
gpui-html check .tmp/gpui-html/mockup.html \
  --manifest .tmp/gpui-html/theme.toml \
  --format json
```

Exit codes: `0` success, `1` compile error (parse / class / CSS),
`2` usage error (missing file, bad manifest).

### 4. Iterate on the manifest, not on the HTML

If lowering fails with `UnknownThemeToken` or `UnknownClass`:

- `UnknownThemeToken` → the token isn't in the manifest or the live
  `Theme`. Add it to both, or change the mockup to use an existing
  token.
- `UnknownClass` with a "palette" hint → the mockup is using a raw
  Tailwind palette (e.g. `bg-red-500`). Replace with a semantic theme
  token (`bg-rose`, `bg-status-error`) added to `Theme`.
- `UnknownClass` with a "needs manifest" hint → custom-scale sizing
  (e.g. `max-w-128`) without a manifest entry. Add it.
- `UnsupportedCssDeclaration` / `UnsupportedCssValue` → the `<style>`
  block uses something gpui-html doesn't lower yet. Either rewrite
  the rule as utility classes (preferred) or accept that the rule
  will need a hand translation during integration.

Resist the urge to "fix" lowering errors by editing
`crates/ato-desktop/`. The fixes belong in the mockup, the manifest,
or upstream in `gpui-html`'s spec — never in this crate.

### 5. Integrate into a production component

Open `.tmp/gpui-html/mockup.rs` and the target file under
`src/ui/<area>/`. The generated source is a single expression chain
like:

```rust
div()
    .flex()
    .flex_col()
    .gap_2()
    .bg(theme.surface)
    .child(div().text_color(theme.body).child("Hello"))
```

Port this into a `Render` impl by hand. Tasks at this step:

- Replace `theme.<token>` reads with the live `Theme` struct in scope
  (`super::theme::Theme` is the canonical import path).
- Replace placeholder text/image content with real state-driven
  bindings (`task.title`, `task.favicon`, …).
- Add `InteractiveElement` wrappers (`on_click`, `on_mouse_down`) for
  any element the mockup represents as interactive. Hover/group
  behavior, drag-and-drop, keyboard focus all live here — gpui-html
  doesn't emit them.
- Split repeated chains into helper functions or `IntoElement` impls
  if the production component is large enough to warrant it.
- Where the generated chain doesn't translate cleanly (e.g. mockup
  uses an `<img>` placeholder where production uses `gpui_component::
  Icon`), prefer the production primitive over forcing the generated
  shape.

Delete `.tmp/gpui-html/mockup.rs` once the production component is
in place — it's not a source of truth, only an intermediate artifact.

### 6. Verify

Run the standard checks for the touched module:

```bash
cargo check
cargo clippy
cargo test
```

If the component renders, smoke-test it via `cargo run` and confirm
visual parity with the mockup. The mockup HTML can be opened in any
browser side-by-side — that's why `mockup.html` and `desktop-mock.html`
live in this crate's root.

## When to update / extend `gpui-html`

If you find yourself repeatedly hand-translating the same construct
out of the generated code (e.g. a particular shadow utility, an
event handler attribute, a layout primitive), that's signal for an
upstream feature request in `ato-run/gpui-html`. Open an issue there
with a minimal repro before working around it locally.

If the mockup uses a feature gpui-html silently passes through
(e.g. a CSS rule in lenient mode), the production component must
translate it by hand — don't claim "the compiler handled it".

## Quick reference

| Action | Command |
|--------|---------|
| Install CLI | `cargo install --git https://github.com/ato-run/gpui-html gpui-html` |
| Compile HTML | `gpui-html compile <html> --manifest <toml> -o <out.rs>` |
| Check (no output) | `gpui-html check <html> --manifest <toml>` |
| JSON diagnostics | `gpui-html check <html> --manifest <toml> --format json` |
| Scratch dir | `.tmp/gpui-html/` (gitignored) |
| Theme fields | `src/ui/theme.rs` |
| Production UI root | `src/ui/` |
