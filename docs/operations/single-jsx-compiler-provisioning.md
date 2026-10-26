# Provisioning the `single-jsx/v1` compiler

`single-jsx/v1` compiles one authored `.jsx` file into a static document with
the **network denied**. That is only true because the compiler and the React it
links are already on the builder when the build starts — so putting them there
is a deploy step, not something a build does for itself.

## What goes where

| | |
|---|---|
| Source of truth | `apps/formation-worker/assets/single-jsx/v1/` |
| On the builder | `/opt/ato/assets/single-jsx/v1/` |
| Bound into the sandbox | read-only, as `/opt/ato/assets` |
| Node the compiler runs on | `/opt/ato/toolchains/node/<SINGLE_JSX_NODE_VERSION>` |

`compile.mjs`, `index.html.tmpl` and `VENDOR.lock` are committed. The three
vendored files are **not**: 3 MB of minified vendor JS would be reviewed by
nobody and diffed by everybody. `VENDOR.lock` pins their exact versions, URLs
and SHA-256 digests, and `provision.sh` refuses anything that does not match.

The compiler is JavaScript, so it also needs the Node named by
`SINGLE_JSX_NODE_VERSION` (`lib/formation/src/preset.rs`). The build may not
fetch it — that is the whole point of the preset — so it has to be on the
builder already, at the same path the plan names. As of 2026-10-24 that is
`22.23.4`, the lowest release patched for CVE-2026-111497; see
`docs/ops/cve-2026-111497-node-brotli-20261024.md`.

## Provisioning

On the builder, with a network:

```sh
cd <ato checkout>/apps/formation-worker/assets/single-jsx/v1
./provision.sh                 # defaults to /opt/ato/assets/single-jsx/v1
```

It is idempotent — a file already present with the right digest is left alone —
and it makes the installed tree read-only. A build that could edit the compiler
could change what every later build produces.

## Verifying

```sh
ls /opt/ato/assets/single-jsx/v1
sha256sum /opt/ato/assets/single-jsx/v1/*.js
```

A builder with the assets missing does not fail obscurely: the build step
checks for `compile.mjs` first and reports
`single_jsx_compiler_unavailable`, which reaches the uploader as "this builder
cannot compile a single component right now", and reaches an operator as this
page.

## Changing a pinned version

Do not change the compiler or React. `single-jsx/v1` is a promise that a source
formed under it keeps meaning what it meant, and every artifact already formed
under it recorded that Babel and that React in its intent. A different compiler
or a different React is `single-jsx/v2`, added beside this one.

The Node the compiler runs on is not part of that promise in the same way: it is
a platform build-toolchain pin, and a security fix may move it (for example the
CVE-2026-111497 bump from `20.20.2` to `22.23.4`). The exact version is recorded
in each plan and derivation, so new formations name the new Node while a
previously sealed derivation keeps the one it pinned. Bumping it still changes
the plan and D of new `single-jsx/v1` formations, so ship the Node to the
builders before the code that names it.

## Testing the compiler

```sh
cd apps/formation-worker/assets/single-jsx/v1
./provision.sh .               # fetch the pinned bytes into this directory
node --test compile.test.mjs
```

Not a cargo test: it exercises JavaScript running on the Node a builder
provisions, and a Rust test could only assert that something was shelled out to.
