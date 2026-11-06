#!/usr/bin/env node
/**
 * Generate and verify the worker binary distribution manifest.
 *
 * ISSUE-27: a distributed worker binary must be identifiable by a version pin
 * (40-character git commit + package version) and by a SHA-256 checksum, and
 * must be installable reproducibly. This tool has two commands:
 *
 *   generate  scan <dist>/<target-triple>/<binary> and write worker-manifest.json
 *   verify    check an existing manifest against a distribution directory
 *
 * It is deliberately dependency-free (Node.js built-ins only) and never uses
 * the network. A checksum manifest detects corruption and accidental swaps; it
 * does not authenticate a publisher. Compare the commit/version/SHA-256 with
 * the release record obtained through a trusted channel before trusting it.
 *
 * Exit codes: 0 success, 1 verification failure, 2 usage error.
 *
 * Run the self-tests with:
 *   node --test scripts/release/*.test.mjs
 */
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

export const SCHEMA = "ato.worker-binary-manifest/1";
export const DEFAULT_BINARY = "ato-connected-realization-worker";

/**
 * Target triples this repository can build. Extending the list is a deliberate
 * change; an unknown triple fails closed instead of being silently accepted.
 */
export const SUPPORTED_TRIPLES = Object.freeze([
  "aarch64-apple-darwin",
  "aarch64-unknown-linux-gnu",
  "x86_64-apple-darwin",
  "x86_64-pc-windows-msvc",
  "x86_64-unknown-linux-gnu",
]);

const COMMIT_RE = /^[0-9a-f]{40}$/;
const VERSION_RE = /^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;
const SHA256_RE = /^[0-9a-f]{64}$/;
const GENERATED_AT_RE = /^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$/;
const BINARY_RE = /^[A-Za-z0-9._-]+$/;
const FILE_RE = /^[a-z0-9_-]+\/[A-Za-z0-9._-]+$/;

const USAGE = `Usage: node scripts/release/worker-manifest.mjs <command> [options]

Commands:
  generate   Build worker-manifest.json from a distribution directory.
  verify     Verify a distribution directory against an existing manifest.

generate options:
  --dist DIR             Distribution root containing <triple>/<binary>.
  --out FILE             Manifest path (may live inside --dist).
  --commit HEX           Required. 40 lowercase hex git commit that was built.
  --version SEMVER       Required. ato release version the binary belongs to.
  --generated-at TIME    Optional RFC 3339 UTC (YYYY-MM-DDTHH:MM:SSZ); also
                         read from SOURCE_DATE_EPOCH. Defaults to now.
  --binary NAME          Binary name (default: ${DEFAULT_BINARY}).
  --require-triple T     Repeatable. Fail when T is not present.
  --check                Compare with the existing --out file instead of writing.

verify options:
  --dist DIR             Distribution root containing <triple>/<binary>.
  --manifest FILE        Manifest to verify.
  --binary NAME          Binary name (default: ${DEFAULT_BINARY}).
  --require-triple T     Repeatable. Fail when T is not present.
  --commit HEX           Optional expected commit; mismatch fails.
  --version SEMVER       Optional expected version; mismatch fails.

Fail codes include: unpinned_commit, unpinned_version, manifest_unknown_field,
unsupported_triple, unknown_file, missing_target, undeclared_target,
sha256_mismatch, size_mismatch, pin_mismatch, manifest_drift, no_targets.
`;

const HELP_WORDS = new Set(["help", "--help", "-h"]);

class Failure extends Error {
  constructor(code, message, exitCode = 1) {
    super(message);
    this.name = "Failure";
    this.code = code;
    this.exitCode = exitCode;
  }
}

function fail(code, message, exitCode = 1) {
  throw new Failure(code, message, exitCode);
}

function usage(message) {
  return new Failure("usage", message, 2);
}

function requireValue(argv, index, option) {
  const value = argv[index];
  if (value === undefined || value.startsWith("--")) {
    throw usage(`${option} requires a value`);
  }
  return value;
}

function parseArgs(argv) {
  const command = argv[0];
  if (command === undefined || HELP_WORDS.has(command)) {
    return { command: "help" };
  }
  if (command !== "generate" && command !== "verify") {
    throw usage(`unknown command: ${command}`);
  }
  const options = {
    command,
    check: false,
    requireTriples: [],
    binary: DEFAULT_BINARY,
    commit: undefined,
    version: undefined,
    generatedAt: undefined,
    dist: undefined,
    out: undefined,
    manifest: undefined,
  };
  for (let index = 1; index < argv.length; index += 1) {
    const arg = argv[index];
    switch (arg) {
      case "--dist":
        options.dist = requireValue(argv, ++index, arg);
        break;
      case "--out":
        options.out = requireValue(argv, ++index, arg);
        break;
      case "--manifest":
        options.manifest = requireValue(argv, ++index, arg);
        break;
      case "--commit":
        options.commit = requireValue(argv, ++index, arg);
        break;
      case "--version":
        options.version = requireValue(argv, ++index, arg);
        break;
      case "--generated-at":
        options.generatedAt = requireValue(argv, ++index, arg);
        break;
      case "--binary":
        options.binary = requireValue(argv, ++index, arg);
        break;
      case "--require-triple":
        options.requireTriples.push(requireValue(argv, ++index, arg));
        break;
      case "--check":
        options.check = true;
        break;
      case "--help":
      case "-h":
        return { command: "help" };
      default:
        throw usage(arg.startsWith("--") ? `unknown option: ${arg}` : `unexpected argument: ${arg}`);
    }
  }
  return options;
}

function normalizeCommit(value, code = "unpinned_commit") {
  if (typeof value !== "string" || value.length === 0) {
    fail(code, "the git commit pin is missing (expected 40 lowercase hex characters)");
  }
  if (!COMMIT_RE.test(value)) {
    fail(code, `commit ${JSON.stringify(value)} is not a 40-character lowercase hex git commit`);
  }
  return value;
}

function normalizeVersion(value, code = "unpinned_version") {
  if (typeof value !== "string" || value.length === 0) {
    fail(code, "the release version pin is missing");
  }
  if (!VERSION_RE.test(value)) {
    fail(code, `version ${JSON.stringify(value)} is not a SemVer version such as 0.8.1`);
  }
  return value;
}

function normalizeBinary(value) {
  if (typeof value !== "string" || !BINARY_RE.test(value)) {
    fail("invalid_binary", `binary name ${JSON.stringify(value)} is not a plain file name`);
  }
  return value;
}

function normalizeTriples(values) {
  const seen = new Set();
  for (const triple of values) {
    if (!SUPPORTED_TRIPLES.includes(triple)) {
      fail("unsupported_triple", `${JSON.stringify(triple)} is not a supported target triple (${SUPPORTED_TRIPLES.join(", ")})`);
    }
    seen.add(triple);
  }
  return [...seen].sort(compareStrings);
}

function compareStrings(left, right) {
  if (left < right) return -1;
  if (left > right) return 1;
  return 0;
}

function normalizeGeneratedAt(options) {
  if (options.generatedAt !== undefined) {
    if (!GENERATED_AT_RE.test(options.generatedAt)) {
      throw usage("--generated-at must be RFC 3339 UTC in the form YYYY-MM-DDTHH:MM:SSZ");
    }
    return options.generatedAt;
  }
  const epoch = process.env.SOURCE_DATE_EPOCH;
  if (epoch !== undefined && epoch !== "") {
    if (!/^[0-9]+$/.test(epoch)) {
      throw usage("SOURCE_DATE_EPOCH must be an integer number of seconds");
    }
    return new Date(Number(epoch) * 1000).toISOString().replace(/\.000Z$/, "Z");
  }
  return new Date().toISOString().replace(/\.\d{3}Z$/, "Z");
}

async function sha256File(filePath) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(filePath)) {
    hash.update(chunk);
  }
  return hash.digest("hex");
}

function readDirents(dirPath) {
  return fs
    .readdirSync(dirPath, { withFileTypes: true })
    .slice()
    .sort((left, right) => compareStrings(left.name, right.name));
}

/**
 * Scan a distribution directory. Layout is fixed:
 *   <dist>/<target-triple>/<binary>
 * Anything else fails closed so an unexpected artifact cannot be shipped under
 * an apparently valid manifest.
 */
async function scanDistribution({ distDir, binary, manifestPath, requireTriples }) {
  const distAbs = path.resolve(distDir);
  let distStat;
  try {
    distStat = fs.lstatSync(distAbs);
  } catch {
    fail("dist_missing", `distribution directory ${JSON.stringify(distDir)} does not exist`);
  }
  if (distStat.isSymbolicLink() || !distStat.isDirectory()) {
    fail("dist_not_directory", `distribution path ${JSON.stringify(distDir)} is not a directory`);
  }
  const manifestAbs = manifestPath === undefined ? undefined : path.resolve(manifestPath);
  const actual = new Map();
  for (const entry of readDirents(distAbs)) {
    const entryAbs = path.join(distAbs, entry.name);
    if (manifestAbs !== undefined && entryAbs === manifestAbs) {
      continue;
    }
    if (entry.isSymbolicLink()) {
      fail("unsafe_entry", `refusing symlink ${entry.name} in the distribution directory`);
    }
    if (!entry.isDirectory()) {
      fail("unknown_file", `unexpected file ${entry.name}; expected <target-triple>/${binary}`);
    }
    if (!SUPPORTED_TRIPLES.includes(entry.name)) {
      fail("unsupported_triple", `directory ${entry.name} is not a supported target triple`);
    }
    for (const inner of readDirents(entryAbs)) {
      const relative = `${entry.name}/${inner.name}`;
      if (inner.isSymbolicLink()) {
        fail("unsafe_entry", `refusing symlink ${relative} in the distribution directory`);
      }
      if (!inner.isFile() || inner.name !== binary) {
        fail("unknown_file", `unexpected entry ${relative}; expected ${entry.name}/${binary}`);
      }
    }
    const fileAbs = path.join(entryAbs, binary);
    let fileStat;
    try {
      fileStat = fs.lstatSync(fileAbs);
    } catch {
      fail("missing_target", `no binary for target ${entry.name} (expected ${entry.name}/${binary})`);
    }
    if (fileStat.isSymbolicLink() || !fileStat.isFile()) {
      fail("unsafe_entry", `${entry.name}/${binary} is not a regular file`);
    }
    actual.set(entry.name, {
      triple: entry.name,
      file: `${entry.name}/${binary}`,
      sha256: await sha256File(fileAbs),
      size: fileStat.size,
    });
  }
  if (actual.size === 0) {
    fail("no_targets", `no ${binary} binaries found under ${JSON.stringify(distDir)}`);
  }
  for (const triple of requireTriples) {
    if (!actual.has(triple)) {
      fail("required_target_missing", `required target ${triple} is not present in the distribution directory`);
    }
  }
  return actual;
}

function validateManifest(manifest) {
  if (manifest === null || typeof manifest !== "object" || Array.isArray(manifest)) {
    fail("manifest_invalid", "manifest must be a JSON object");
  }
  const allowed = new Set(["schema", "ato_version", "git_commit", "generated_at", "targets"]);
  for (const key of Object.keys(manifest)) {
    if (!allowed.has(key)) {
      fail("manifest_unknown_field", `manifest has unknown field ${JSON.stringify(key)}`);
    }
  }
  for (const key of allowed) {
    if (!(key in manifest)) {
      fail("manifest_missing_field", `manifest is missing required field ${JSON.stringify(key)}`);
    }
  }
  if (manifest.schema !== SCHEMA) {
    fail("unsupported_schema", `manifest schema ${JSON.stringify(manifest.schema)} is not ${JSON.stringify(SCHEMA)}`);
  }
  const commit = normalizeCommit(manifest.git_commit);
  const version = normalizeVersion(manifest.ato_version);
  if (typeof manifest.generated_at !== "string" || !GENERATED_AT_RE.test(manifest.generated_at)) {
    fail("manifest_invalid", "generated_at must be RFC 3339 UTC in the form YYYY-MM-DDTHH:MM:SSZ");
  }
  if (!Array.isArray(manifest.targets) || manifest.targets.length === 0) {
    fail("manifest_invalid", "targets must be a non-empty array");
  }
  const triples = [];
  for (const target of manifest.targets) {
    if (target === null || typeof target !== "object" || Array.isArray(target)) {
      fail("manifest_invalid", "every targets entry must be an object");
    }
    const targetKeys = new Set(["triple", "file", "sha256", "size"]);
    for (const key of Object.keys(target)) {
      if (!targetKeys.has(key)) {
        fail("manifest_unknown_field", `target entry has unknown field ${JSON.stringify(key)}`);
      }
    }
    for (const key of targetKeys) {
      if (!(key in target)) {
        fail("manifest_missing_field", `target entry is missing required field ${JSON.stringify(key)}`);
      }
    }
    if (!SUPPORTED_TRIPLES.includes(target.triple)) {
      fail("unsupported_triple", `manifest declares unsupported target triple ${JSON.stringify(target.triple)}`);
    }
    if (typeof target.file !== "string" || !FILE_RE.test(target.file) || target.file.includes("..")) {
      fail("manifest_invalid", `target ${target.triple} has an unsafe file path ${JSON.stringify(target.file)}`);
    }
    if (target.file.split("/")[0] !== target.triple) {
      fail("manifest_invalid", `target ${target.triple} file path ${JSON.stringify(target.file)} does not start with its triple`);
    }
    if (typeof target.sha256 !== "string" || !SHA256_RE.test(target.sha256)) {
      fail("manifest_invalid", `target ${target.triple} sha256 is not 64 lowercase hex characters`);
    }
    if (!Number.isSafeInteger(target.size) || target.size < 0) {
      fail("manifest_invalid", `target ${target.triple} size is not a non-negative integer`);
    }
    triples.push(target.triple);
  }
  if (new Set(triples).size !== triples.length) {
    fail("manifest_invalid", "targets contains duplicate triples");
  }
  const sorted = triples.slice().sort(compareStrings);
  if (sorted.some((triple, index) => triple !== triples[index])) {
    fail("manifest_unsorted", "targets must be sorted by target triple");
  }
  return { commit, version };
}

function renderManifest(manifest) {
  return `${JSON.stringify(manifest, null, 2)}\n`;
}

function buildManifest({ commit, version, generatedAt, targets }) {
  return {
    schema: SCHEMA,
    ato_version: version,
    git_commit: commit,
    generated_at: generatedAt,
    targets: [...targets].sort((left, right) => compareStrings(left.triple, right.triple)),
  };
}

function writeFileAtomic(filePath, contents) {
  const absolute = path.resolve(filePath);
  fs.mkdirSync(path.dirname(absolute), { recursive: true });
  const temporary = `${absolute}.tmp-${process.pid}`;
  try {
    fs.writeFileSync(temporary, contents, { mode: 0o644 });
    fs.renameSync(temporary, absolute);
  } catch (error) {
    fs.rmSync(temporary, { force: true });
    throw error;
  }
}

async function generate(options) {
  if (options.dist === undefined) throw usage("generate requires --dist");
  if (options.out === undefined) throw usage("generate requires --out");
  if (options.commit === undefined) throw usage("generate requires --commit");
  if (options.version === undefined) throw usage("generate requires --version");
  const binary = normalizeBinary(options.binary);
  const requireTriples = normalizeTriples(options.requireTriples);
  const commit = normalizeCommit(options.commit);
  const version = normalizeVersion(options.version);
  const generatedAt = normalizeGeneratedAt(options);
  const outAbs = path.resolve(options.out);
  const actual = await scanDistribution({
    distDir: options.dist,
    binary,
    manifestPath: outAbs,
    requireTriples,
  });
  const manifest = buildManifest({
    commit,
    version,
    generatedAt,
    targets: [...actual.values()],
  });
  const text = renderManifest(manifest);

  if (options.check) {
    let existingText;
    try {
      existingText = fs.readFileSync(outAbs, "utf8");
    } catch {
      fail("manifest_missing", `--check requires an existing manifest at ${JSON.stringify(options.out)}`);
    }
    let existing;
    try {
      existing = JSON.parse(existingText);
    } catch {
      fail("manifest_invalid_json", `${JSON.stringify(options.out)} is not valid JSON`);
    }
    const existingPin = validateManifest(existing);
    if (commit !== existingPin.commit) {
      fail("pin_mismatch", `${JSON.stringify(options.out)} is pinned to commit ${existingPin.commit}, not ${commit}`);
    }
    if (version !== existingPin.version) {
      fail("pin_mismatch", `${JSON.stringify(options.out)} is pinned to version ${existingPin.version}, not ${version}`);
    }
    const pinnedTimestamp =
      options.generatedAt !== undefined ||
      (process.env.SOURCE_DATE_EPOCH !== undefined && process.env.SOURCE_DATE_EPOCH !== "");
    const comparableExisting = pinnedTimestamp ? existing : { ...existing, generated_at: manifest.generated_at };
    if (JSON.stringify(comparableExisting) !== JSON.stringify(manifest)) {
      fail("manifest_drift", `${JSON.stringify(options.out)} does not match the distribution directory; regenerate it`);
    }
    process.stdout.write(`worker-manifest: ok: ${options.out} matches ${actual.size} target(s) (commit ${commit}, version ${version})\n`);
    return 0;
  }

  writeFileAtomic(outAbs, text);
  process.stdout.write(
    `worker-manifest: wrote ${options.out}: ${actual.size} target(s), commit ${commit}, version ${version}\n`,
  );
  return 0;
}

async function verify(options) {
  if (options.dist === undefined) throw usage("verify requires --dist");
  if (options.manifest === undefined) throw usage("verify requires --manifest");
  const binary = normalizeBinary(options.binary);
  const requireTriples = normalizeTriples(options.requireTriples);
  const expectedCommit = options.commit === undefined ? undefined : normalizeCommit(options.commit);
  const expectedVersion = options.version === undefined ? undefined : normalizeVersion(options.version);

  let text;
  try {
    text = fs.readFileSync(path.resolve(options.manifest), "utf8");
  } catch {
    fail("manifest_missing", `manifest ${JSON.stringify(options.manifest)} does not exist`);
  }
  let manifest;
  try {
    manifest = JSON.parse(text);
  } catch {
    fail("manifest_invalid_json", `${JSON.stringify(options.manifest)} is not valid JSON`);
  }
  const pinned = validateManifest(manifest);

  if (expectedCommit !== undefined && expectedCommit !== pinned.commit) {
    fail("pin_mismatch", `manifest commit ${pinned.commit} does not match the expected pin ${expectedCommit}`);
  }
  if (expectedVersion !== undefined && expectedVersion !== pinned.version) {
    fail("pin_mismatch", `manifest version ${pinned.version} does not match the expected pin ${expectedVersion}`);
  }

  const declared = new Map();
  for (const target of manifest.targets) {
    if (target.file !== `${target.triple}/${binary}`) {
      fail(
        "file_path_mismatch",
        `manifest target ${target.triple} declares ${JSON.stringify(target.file)}, expected ${JSON.stringify(`${target.triple}/${binary}`)}`,
      );
    }
    declared.set(target.triple, target);
  }

  const actual = await scanDistribution({
    distDir: options.dist,
    binary,
    manifestPath: path.resolve(options.manifest),
    requireTriples,
  });

  for (const [triple, target] of declared) {
    const found = actual.get(triple);
    if (found === undefined) {
      fail("missing_target", `manifest declares ${triple} but ${triple}/${binary} is absent from ${JSON.stringify(options.dist)}`);
    }
    if (found.size !== target.size) {
      fail(
        "size_mismatch",
        `${triple}/${binary} is ${found.size} bytes, manifest declares ${target.size}`,
      );
    }
    if (found.sha256 !== target.sha256) {
      fail(
        "sha256_mismatch",
        `${triple}/${binary} SHA-256 is ${found.sha256}, manifest declares ${target.sha256}`,
      );
    }
  }
  for (const triple of actual.keys()) {
    if (!declared.has(triple)) {
      fail("undeclared_target", `${triple}/${binary} is present in the distribution directory but absent from the manifest`);
    }
  }

  process.stdout.write(
    `worker-manifest: verified ${options.manifest}: ${declared.size} target(s), commit ${pinned.commit}, version ${pinned.version}\n`,
  );
  return 0;
}

function checkOptionApplicability(options) {
  if (options.command === "generate") {
    if (options.manifest !== undefined) throw usage("--manifest is only valid with verify");
    return;
  }
  if (options.out !== undefined) throw usage("--out is only valid with generate");
  if (options.check) throw usage("--check is only valid with generate");
  if (options.generatedAt !== undefined) throw usage("--generated-at is only valid with generate");
}

async function main(argv) {
  const options = parseArgs(argv);
  if (options.command === "help") {
    process.stdout.write(USAGE);
    return 0;
  }
  checkOptionApplicability(options);
  if (options.command === "generate") {
    return generate(options);
  }
  return verify(options);
}

const invokedPath = process.argv[1] === undefined ? "" : pathToFileURL(path.resolve(process.argv[1])).href;
if (invokedPath === import.meta.url) {
  main(process.argv.slice(2))
    .then((code) => {
      process.exitCode = code;
    })
    .catch((error) => {
      if (error instanceof Failure) {
        process.stderr.write(`worker-manifest: error: ${error.code}: ${error.message}\n`);
        process.exitCode = error.exitCode;
        return;
      }
      process.stderr.write(`worker-manifest: error: internal: ${error && error.stack ? error.stack : String(error)}\n`);
      process.exitCode = 1;
    });
}

export { buildManifest, scanDistribution, validateManifest, verify };
