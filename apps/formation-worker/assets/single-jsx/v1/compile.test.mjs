/**
 * What `single-jsx/v1` accepts and what it refuses BY NAME.
 *
 * Run with `node --test` from this directory, after `./provision.sh .` has put
 * the pinned vendor bytes here. It is not a cargo test on purpose: it exercises
 * the compiler, and the compiler is JavaScript that runs on a Node the builder
 * provisions — a Rust test would only be able to assert that it shelled out.
 *
 * The refusals matter more than the successes. A build that silently drops an
 * import produces an app that fails in the browser, at the person using it,
 * with nothing on screen to explain why.
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));

function compile(source, name = "App.jsx") {
  const dir = mkdtempSync(join(tmpdir(), "single-jsx-"));
  writeFileSync(join(dir, name), source, "utf8");
  try {
    execFileSync(
      process.execPath,
      [join(HERE, "compile.mjs"), "--entry", name, "--out", "dist"],
      { cwd: dir, stdio: ["ignore", "pipe", "pipe"] },
    );
  } catch (error) {
    const stderr = String(error.stderr ?? "");
    const line = stderr
      .split("\n")
      .find((l) => l.startsWith("ATO_FORMATION_FAILURE "));
    return {
      ok: false,
      failure: line ? JSON.parse(line.slice("ATO_FORMATION_FAILURE ".length)) : null,
      stderr,
    };
  }
  const html = readFileSync(join(dir, "dist/index.html"), "utf8");
  const src = (pattern) => (html.match(pattern) || [])[0];
  const appPath = src(/app\.[0-9a-f]+\.js/);
  const reactPath = src(/vendor\/react\.[0-9a-f]+\.js/);
  return {
    ok: true,
    html,
    appPath,
    reactPath,
    app: readFileSync(join(dir, "dist", appPath), "utf8"),
    react: readFileSync(join(dir, "dist", reactPath), "utf8"),
  };
}

const HAIKU = `
import React, { useState } from "react";
const KEY = "haikukai:v1";
export default function HaikuKai() {
  const [draft, setDraft] = useState("");
  return <main title={KEY} onClick={() => setDraft("")}>{draft}</main>;
}
`;

test("compiles one component and links the pinned React", () => {
  const result = compile(HAIKU);
  assert.equal(result.ok, true);
  // The import became a binding to the global the vendor script defines —
  // there is no module loader in the page.
  assert.match(result.app, /var React = globalThis\.React;/);
  assert.match(result.app, /var useState = globalThis\.React\.useState;/);
  assert.doesNotMatch(result.app, /^import /m);
  // JSX is gone, and the classic runtime is what replaced it: `automatic`
  // would emit an import this compiler then has to refuse — its own output
  // failing its own policy.
  assert.doesNotMatch(result.app, /<main/);
  assert.match(result.app, /React\.createElement/);
  // Mounted from the DEFAULT export, under its own name.
  assert.match(result.app, /createElement\(HaikuKai\)/);
  assert.match(result.html, /<div id="root"><\/div>/);
  assert.ok(result.appPath, "the document references a hashed app bundle");
  assert.match(result.html, /<title>HaikuKai<\/title>/);
  assert.match(result.react, /react\.production\.min\.js|Symbol\.for/);
});

test("binds React even when the author never imported it", () => {
  // A Claude Artifact often does not, and the JSX this compiler emits needs it.
  const result = compile(`export default function A(){ return <p>hi</p>; }`);
  assert.equal(result.ok, true);
  assert.match(result.app, /var React = globalThis\.React;/);
});

test("keeps the author's own name for the default import", () => {
  const result = compile(
    `import R from "react"; export default function A(){ return R.createElement("p"); }`,
  );
  assert.equal(result.ok, true);
  assert.match(result.app, /var R = globalThis\.React;/);
});

test("escapes the title rather than trusting a component name", () => {
  // Authored text going into markup. The name cannot contain a `<` today, but
  // the escaping is where it belongs rather than where it is convenient.
  const result = compile(HAIKU);
  assert.doesNotMatch(result.html, /<title>[^<]*<[^/]/);
});

const REFUSALS = [
  ["single_jsx_unsupported_package", `import _ from "lodash"; export default function A(){return <p/>;}`],
  ["single_jsx_remote_import", `import x from "https://esm.sh/x"; export default function A(){return <p/>;}`],
  ["single_jsx_relative_import", `import x from "./helpers"; export default function A(){return <p/>;}`],
  ["single_jsx_dynamic_import", `export default function A(){ import("./y"); return <p/>;}`],
  ["single_jsx_require_unsupported", `const y = require("fs"); export default function A(){return <p/>;}`],
  // A computed require is the case a regex over `require(` cannot tell from a
  // literal one — and the difference is the whole question.
  ["single_jsx_require_unsupported", `const n = "f"+"s"; const y = require(n); export default function A(){return <p/>;}`],
  ["single_jsx_unsupported_export", `export const z = 1; export default function A(){return <p/>;}`],
  ["single_jsx_no_default_export", `function A(){return <p/>;}`],
  ["single_jsx_syntax_error", `export default function A(){return <p/;}`],
];

for (const [code, source] of REFUSALS) {
  test(`refuses by name: ${code} — ${source.slice(0, 34)}…`, () => {
    const result = compile(source);
    assert.equal(result.ok, false, "should not have compiled");
    assert.notEqual(result.failure, null, `no typed failure in: ${result.stderr}`);
    assert.equal(result.failure.code, code);
    // The sentence is for the person who uploaded the file, so it says what to
    // do — never a stack, a path, or a module resolution trace.
    assert.ok(result.failure.message.length > 20);
    assert.doesNotMatch(result.failure.message, /node_modules|\/tmp\/|at Object\./);
  });
}

/**
 * The app host serves instance assets `immutable` with a one-year max-age,
 * which is correct only while a URL's bytes never change. A fixed `app.js`
 * broke that: after an update the document is fresh (`no-store`) and points at
 * the same `/app.js`, so a returning browser keeps running last year's code.
 * Measured on staging — a patched source built, published and adopted, and the
 * page still rendered the old title.
 */
test("a changed source produces a changed app URL", () => {
  const first = compile(HAIKU);
  const edited = compile(HAIKU.replace("HaikuKai", "HaikuKaiTwo"));
  assert.notEqual(first.appPath, edited.appPath);
  // React did not change, so its URL does not either — it stays cached across
  // the update, which is the other half of why hashing is the right fix.
  assert.equal(first.reactPath, edited.reactPath);
  // Same bytes, same name: a rebuild of an unchanged source is not a new URL.
  assert.equal(compile(HAIKU).appPath, first.appPath);
});

test("react-dom is linkable, because this compiler ships it", () => {
  const result = compile(
    `import ReactDOM from "react-dom"; export default function A(){ return <p>{typeof ReactDOM}</p>; }`,
  );
  assert.equal(result.ok, true);
  assert.match(result.app, /var ReactDOM = globalThis\.ReactDOM;/);
});
