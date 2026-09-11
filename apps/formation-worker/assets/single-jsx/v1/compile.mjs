/**
 * `single-jsx/v1` — one authored `.jsx` file into a static document.
 *
 * ## What this is, and what it deliberately is not
 *
 * It is a PLATFORM-MANAGED compiler: pinned, shipped with the builder, run
 * with the network denied, and versioned in the same breath as the preset that
 * names it. `single-jsx/v1` means one thing forever; a different Babel or a
 * different React is `v2`.
 *
 * It is not a bundler. There is no resolver, no package graph and no cache,
 * because the input is one file and the only runtime it may link is the React
 * this directory already holds. Anything else the file asks for is refused BY
 * NAME rather than dropped — a build that silently ignores an import produces
 * an app that fails in the browser, at the user, with no explanation.
 *
 * ## Why the AST and not a regex
 *
 * Import policy is decided on parsed syntax. A regex over `require(` cannot
 * tell a call from a string, cannot see `import()`, and cannot distinguish
 * `require(name)` from `require("react")` — and the difference between those
 * two is the whole question this file exists to answer.
 *
 * ## Failures
 *
 * Every refusal prints one `ATO_FORMATION_FAILURE <json>` line to stderr and
 * exits 65. The worker reads that line and reports the code and the sentence;
 * nothing else from this process reaches whoever uploaded the source.
 */
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const EXIT_REFUSED = 65;

/** The only module specifiers this compiler can satisfy from its own assets. */
const LINKABLE = new Set(["react", "react-dom", "react-dom/client"]);

function refuse(code, message) {
  process.stderr.write(
    `ATO_FORMATION_FAILURE ${JSON.stringify({ code, stage: "build", message })}\n`,
  );
  process.exit(EXIT_REFUSED);
}

function arg(name) {
  const at = process.argv.indexOf(`--${name}`);
  return at === -1 ? null : process.argv[at + 1];
}

const entry = arg("entry");
const out = arg("out");
if (!entry || !out) {
  // An operator error, not an uploader's: the build plan names both.
  process.stderr.write("usage: compile.mjs --entry <file.jsx> --out <dir>\n");
  process.exit(2);
}

const Babel = (await import(join(HERE, "babel.standalone.js"))).default
  ?? globalThis.Babel;
const { parser, types: t } = Babel.packages;

let source;
try {
  source = readFileSync(resolve(entry), "utf8");
} catch {
  refuse(
    "single_jsx_entry_unreadable",
    `Ato could not read ${entry}. Upload the .jsx file itself, not a folder.`,
  );
}

let ast;
try {
  ast = parser.parse(source, {
    sourceType: "module",
    plugins: ["jsx"],
    errorRecovery: false,
  });
} catch (error) {
  // The parser's own position is the useful part and carries no path.
  const at = error.loc ? ` (line ${error.loc.line}, column ${error.loc.column})` : "";
  refuse(
    "single_jsx_syntax_error",
    `This file is not valid JSX${at}. ${String(error.message).split("\n")[0]}`,
  );
}

/**
 * The import policy, and the prologue it produces.
 *
 * React is a global here, not a module, so an accepted import becomes a
 * binding to that global. The specifiers are collected rather than assumed:
 * `import React, { useState } from "react"` and `import R from "react"` are
 * both ordinary, and guessing the local names would break one of them.
 */
const prologue = [];
const seenDefaults = new Set();

function bindGlobal(local, globalName, path) {
  if (path === undefined) {
    prologue.push(`var ${local} = globalThis.${globalName};`);
  } else {
    prologue.push(`var ${local} = globalThis.${globalName}.${path};`);
  }
}

function globalFor(specifier) {
  return specifier === "react" ? "React" : "ReactDOM";
}

for (const node of ast.program.body) {
  if (node.type !== "ImportDeclaration") continue;
  const from = node.source.value;
  if (/^https?:\/\//.test(from)) {
    refuse(
      "single_jsx_remote_import",
      `This file imports ${from} over the network. A single component is \
compiled offline, so it can only use React.`,
    );
  }
  if (from.startsWith(".") || from.startsWith("/")) {
    refuse(
      "single_jsx_relative_import",
      `This file imports ${from} from another file. Ato compiles ONE .jsx \
file — inline what it needs, or build the project and upload the site.`,
    );
  }
  if (!LINKABLE.has(from)) {
    refuse(
      "single_jsx_unsupported_package",
      `This file imports "${from}", which Ato cannot install. A single \
component may use React only.`,
    );
  }
  const globalName = globalFor(from);
  for (const specifier of node.specifiers) {
    if (specifier.type === "ImportNamespaceSpecifier") {
      bindGlobal(specifier.local.name, globalName);
    } else if (specifier.type === "ImportDefaultSpecifier") {
      seenDefaults.add(specifier.local.name);
      bindGlobal(specifier.local.name, globalName);
    } else {
      bindGlobal(specifier.local.name, globalName, specifier.imported.name);
    }
  }
}

/**
 * Strip the module syntax this compiler has already accounted for, and refuse
 * the syntax it has not.
 *
 * `export default` becomes the app's entry component. Anything else a module
 * can export has no meaning for a file nothing imports, and saying so is
 * better than removing it and leaving the author to wonder.
 */
let defaultExportName = null;
const plugin = {
  visitor: {
    ImportDeclaration(path) {
      path.remove();
    },
    CallExpression(path) {
      // Babel models `import(x)` as a CallExpression whose callee is the
      // `Import` node, not as its own expression type — so checking only for
      // `ImportExpression` sees nothing, which is how a dynamic import gets
      // through a policy that believes it is checking for one.
      if (path.node.callee.type === "Import") {
        refuse(
          "single_jsx_dynamic_import",
          "This file loads a module at runtime with import(). A single \
component is compiled offline and has nothing to load from.",
        );
      }
      if (path.node.callee.type !== "Identifier") return;
      if (path.node.callee.name !== "require") return;
      const first = path.node.arguments[0];
      const named =
        first && first.type === "StringLiteral" ? ` ("${first.value}")` : "";
      refuse(
        "single_jsx_require_unsupported",
        `This file calls require()${named}. A single component is compiled \
offline; use an \`import\` from "react", or nothing.`,
      );
    },
    ExportDefaultDeclaration(path) {
      const declaration = path.node.declaration;
      if (
        (t.isFunctionDeclaration(declaration) || t.isClassDeclaration(declaration)) &&
        declaration.id
      ) {
        defaultExportName = declaration.id.name;
        path.replaceWith(declaration);
        return;
      }
      defaultExportName = "__ato_app_default";
      path.replaceWith(
        t.variableDeclaration("var", [
          t.variableDeclarator(
            t.identifier("__ato_app_default"),
            t.toExpression(declaration),
          ),
        ]),
      );
    },
    ExportNamedDeclaration() {
      refuse(
        "single_jsx_unsupported_export",
        "This file has a named export. Ato renders the DEFAULT export of a \
single component file — mark the component `export default`.",
      );
    },
    ExportAllDeclaration() {
      refuse(
        "single_jsx_unsupported_export",
        "This file re-exports another module. Ato compiles ONE .jsx file.",
      );
    },
  },
};

let compiled;
try {
  compiled = Babel.transformFromAst(ast, source, {
    ast: false,
    code: true,
    babelrc: false,
    configFile: false,
    compact: false,
    sourceMaps: false,
    // Classic runtime: React is a global, so there is no `jsx` module to
    // import from. `automatic` would emit an import this compiler then has to
    // refuse — its own output failing its own policy.
    presets: [["react", { runtime: "classic" }]],
    plugins: [plugin],
  });
} catch (error) {
  refuse(
    "single_jsx_compile_failed",
    `Ato could not compile this component. ${String(error.message).split("\n")[0]}`,
  );
}

if (!defaultExportName) {
  refuse(
    "single_jsx_no_default_export",
    "Ato renders the default export of a single component file, and this one \
has none. Add `export default` to your component.",
  );
}

// React is bound for the JSX the compiler just emitted, even when the author
// never wrote the import — a Claude Artifact often does not.
if (!prologue.some((line) => /^var React /.test(line))) {
  prologue.unshift("var React = globalThis.React;");
}

const outDir = resolve(out);
mkdirSync(join(outDir, "vendor"), { recursive: true });

const epilogue = `
var __ato_root = document.getElementById("root");
globalThis.ReactDOM.createRoot(__ato_root).render(
  globalThis.React.createElement(${defaultExportName}),
);
`;

/**
 * Every emitted file carries the digest of its own contents in its name.
 *
 * Not decoration. The app host serves instance assets `immutable` with a
 * one-year max-age, which is correct only while a URL's bytes never change.
 * A fixed `app.js` breaks that: after an update the document is fresh (it is
 * served `no-store`) and points at the same `/app.js`, so a returning browser
 * keeps running LAST YEAR'S code. Measured on staging — a patched source
 * built, published and adopted, and the page still rendered the old title.
 *
 * Hashing the name makes the two agree: new bytes are a new URL, and the old
 * URL nobody references any more may be cached forever without harm. React
 * does not change between builds, so its files keep their names and stay
 * cached across an update, which is the other half of why this is right.
 */
function emit(relativePath, contents) {
  const digest = createHash("sha256").update(contents).digest("hex").slice(0, 16);
  const at = relativePath.lastIndexOf(".");
  const hashed = `${relativePath.slice(0, at)}.${digest}${relativePath.slice(at)}`;
  writeFileSync(join(outDir, hashed), contents);
  return hashed;
}

const reactPath = emit(
  "vendor/react.js",
  readFileSync(join(HERE, "react.production.min.js")),
);
const reactDomPath = emit(
  "vendor/react-dom.js",
  readFileSync(join(HERE, "react-dom.production.min.js")),
);
const appPath = emit(
  "app.js",
  `(function () {\n"use strict";\n${prologue.join("\n")}\n${compiled.code}\n${epilogue}})();\n`,
);

/**
 * The document title.
 *
 * Taken from the component's own name, which is the only thing in the file
 * that names it. Escaped rather than trusted: this is authored text going into
 * markup.
 */
const title = defaultExportName === "__ato_app_default" ? "App" : defaultExportName;
const escapeHtml = (text) =>
  text.replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c],
  );
writeFileSync(
  join(outDir, "index.html"),
  readFileSync(join(HERE, "index.html.tmpl"), "utf8")
    .replace("__ATO_TITLE__", escapeHtml(title))
    .replace("__ATO_REACT__", reactPath)
    .replace("__ATO_REACT_DOM__", reactDomPath)
    .replace("__ATO_APP__", appPath),
  "utf8",
);
