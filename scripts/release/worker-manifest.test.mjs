/**
 * Self-tests for the worker binary distribution manifest.
 *
 * Run with: node --test scripts/release/*.test.mjs
 *
 * The refusals are the point. A manifest that silently accepts a tampered
 * binary, an unpinned version, a stray file or a missing target would turn a
 * checksum check into a false assurance. Fixtures are synthesized under the
 * repository .tmp/ directory (never /tmp) and removed afterwards.
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(HERE, "..", "..");
const SCRIPT = join(HERE, "worker-manifest.mjs");
const TMP_ROOT = join(ROOT, ".tmp");
const BINARY = "ato-connected-realization-worker";
const COMMIT = "0123456789abcdef0123456789abcdef01234567";
const OTHER_COMMIT = "fedcba9876543210fedcba9876543210fedcba98";
const VERSION = "0.8.1";
const GENERATED_AT = "2026-11-03T01:22:32Z";

const ENV = { ...process.env };
delete ENV.SOURCE_DATE_EPOCH;

function fixture(t) {
  mkdirSync(TMP_ROOT, { recursive: true });
  const directory = mkdtempSync(join(TMP_ROOT, "worker-manifest-test-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  return directory;
}

function run(args, options = {}) {
  const result = spawnSync(process.execPath, [SCRIPT, ...args], {
    cwd: ROOT,
    encoding: "utf8",
    env: { ...ENV, ...(options.env ?? {}) },
    timeout: 30_000,
  });
  return { status: result.status, stdout: result.stdout, stderr: result.stderr };
}

function writeBinary(dist, triple, contents = `worker-${triple}-v1\n`) {
  const directory = join(dist, triple);
  mkdirSync(directory, { recursive: true });
  const file = join(directory, BINARY);
  writeFileSync(file, contents);
  return file;
}

function generateArgs(dist, extra = []) {
  return [
    "generate",
    "--dist",
    dist,
    "--out",
    join(dist, "worker-manifest.json"),
    "--commit",
    COMMIT,
    "--version",
    VERSION,
    "--generated-at",
    GENERATED_AT,
    ...extra,
  ];
}

function generate(dist, extra = []) {
  return run(generateArgs(dist, extra));
}

function readManifest(dist) {
  return JSON.parse(readFileSync(join(dist, "worker-manifest.json"), "utf8"));
}

function writeManifest(dist, manifest) {
  writeFileSync(join(dist, "worker-manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
}

function assertFailure(result, code) {
  assert.notEqual(result.status, 0, `expected a failure, stdout=${result.stdout}`);
  assert.match(
    result.stderr,
    new RegExp(`worker-manifest: error: ${code}:`),
    `expected error code ${code}, stderr=${result.stderr}`,
  );
}

function lfsTriples(dir) {
  writeBinary(dir, "x86_64-unknown-linux-gnu", "linux-x86_64-binary\n");
  writeBinary(dir, "aarch64-unknown-linux-gnu", "linux-aarch64-binary\n");
}

test("generate writes a sorted, valid, pinned manifest", (t) => {
  const dist = fixture(t);
  lfsTriples(dist);
  const result = generate(dist);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /wrote/);

  const manifest = readManifest(dist);
  assert.deepEqual(Object.keys(manifest), ["schema", "ato_version", "git_commit", "generated_at", "targets"]);
  assert.equal(manifest.schema, "ato.worker-binary-manifest/1");
  assert.equal(manifest.ato_version, VERSION);
  assert.equal(manifest.git_commit, COMMIT);
  assert.equal(manifest.generated_at, GENERATED_AT);
  assert.deepEqual(
    manifest.targets.map((target) => target.triple),
    ["aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu"],
  );
  for (const target of manifest.targets) {
    assert.equal(target.file, `${target.triple}/${BINARY}`);
    assert.match(target.sha256, /^[0-9a-f]{64}$/);
    assert.ok(target.size > 0);
  }
});

test("generate is byte-reproducible for the same pin and timestamp", (t) => {
  const dist = fixture(t);
  lfsTriples(dist);
  assert.equal(generate(dist).status, 0);
  const first = readFileSync(join(dist, "worker-manifest.json"), "utf8");
  assert.equal(generate(dist).status, 0);
  const second = readFileSync(join(dist, "worker-manifest.json"), "utf8");
  assert.equal(first, second);
});

test("generate derives generated_at from SOURCE_DATE_EPOCH", (t) => {
  const dist = fixture(t);
  lfsTriples(dist);
  const epoch = "1762000000";
  const result = run(generateArgs(dist).filter((arg, index, all) => arg !== "--generated-at" && all[index - 1] !== "--generated-at"), {
    env: { SOURCE_DATE_EPOCH: epoch },
  });
  assert.equal(result.status, 0, result.stderr);
  const expected = new Date(Number(epoch) * 1000).toISOString().replace(/\.000Z$/, "Z");
  assert.equal(readManifest(dist).generated_at, expected);
});

test("generate --check accepts a matching manifest and reports drift otherwise", (t) => {
  const dist = fixture(t);
  lfsTriples(dist);
  assert.equal(generate(dist).status, 0);

  const checked = generate(dist, ["--check"]);
  assert.equal(checked.status, 0, checked.stderr);
  assert.match(checked.stdout, /matches/);

  const manifest = readManifest(dist);
  manifest.targets[0].size += 1;
  writeManifest(dist, manifest);
  assertFailure(generate(dist, ["--check"]), "manifest_drift");
});

test("generate --check fails when the pin or a checksum drifted", (t) => {
  const dist = fixture(t);
  lfsTriples(dist);
  assert.equal(generate(dist).status, 0);

  const repinned = generate(dist, ["--check", "--commit", OTHER_COMMIT]);
  assertFailure(repinned, "pin_mismatch");

  const manifest = readManifest(dist);
  manifest.targets[1].sha256 = "0".repeat(64);
  writeManifest(dist, manifest);
  assertFailure(generate(dist, ["--check"]), "manifest_drift");
});

test("generate fails on an unpinned or malformed commit", (t) => {
  const dist = fixture(t);
  lfsTriples(dist);
  const short = run(generateArgs(dist, ["--commit", COMMIT.slice(0, 39)]));
  assertFailure(short, "unpinned_commit");
  const upper = run(generateArgs(dist, ["--commit", COMMIT.toUpperCase()]));
  assertFailure(upper, "unpinned_commit");
  const missing = generateArgs(dist).filter((arg, index, all) => arg !== "--commit" && all[index - 1] !== "--commit");
  assertFailure(run(missing), "usage");
});

test("generate fails on an unpinned or malformed version", (t) => {
  const dist = fixture(t);
  lfsTriples(dist);
  const malformed = run(generateArgs(dist, ["--version", "0.8"]));
  assertFailure(malformed, "unpinned_version");
  const prefixed = run(generateArgs(dist, ["--version", `v${VERSION}`]));
  assertFailure(prefixed, "unpinned_version");
  const missing = generateArgs(dist).filter((arg, index, all) => arg !== "--version" && all[index - 1] !== "--version");
  assertFailure(run(missing), "usage");
});

test("generate fails on unknown files, unsupported triples and a missing target", (t) => {
  const dist = fixture(t);
  lfsTriples(dist);
  writeFileSync(join(dist, "stray.bin"), "stray");
  assertFailure(generate(dist), "unknown_file");
  rmSync(join(dist, "stray.bin"));

  const unsupported = join(dist, "riscv64gc-unknown-linux-gnu");
  mkdirSync(unsupported);
  writeFileSync(join(unsupported, BINARY), "other");
  assertFailure(generate(dist), "unsupported_triple");
  rmSync(unsupported, { recursive: true });

  const emptyTriple = join(dist, "x86_64-apple-darwin");
  mkdirSync(emptyTriple);
  assertFailure(generate(dist), "missing_target");
});

test("verify accepts a matching distribution with the expected pin", (t) => {
  const dist = fixture(t);
  lfsTriples(dist);
  assert.equal(generate(dist).status, 0);
  const result = run([
    "verify",
    "--dist",
    dist,
    "--manifest",
    join(dist, "worker-manifest.json"),
    "--commit",
    COMMIT,
    "--version",
    VERSION,
  ]);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /verified/);
  assert.match(result.stdout, /2 target\(s\)/);
});

test("verify detects a tampered binary by SHA-256", (t) => {
  const dist = fixture(t);
  lfsTriples(dist);
  assert.equal(generate(dist).status, 0);

  const manifest = readManifest(dist);
  const target = manifest.targets.find((entry) => entry.triple === "x86_64-unknown-linux-gnu");
  const file = join(dist, target.file);
  const original = readFileSync(file);
  const tampered = Buffer.from(original);
  tampered[0] ^= 0xff;
  writeFileSync(file, tampered);
  assertFailure(run(["verify", "--dist", dist, "--manifest", join(dist, "worker-manifest.json")]), "sha256_mismatch");
});

test("verify detects a size mismatch", (t) => {
  const dist = fixture(t);
  lfsTriples(dist);
  assert.equal(generate(dist).status, 0);
  const manifest = readManifest(dist);
  manifest.targets[0].size += 1;
  writeManifest(dist, manifest);
  assertFailure(run(["verify", "--dist", dist, "--manifest", join(dist, "worker-manifest.json")]), "size_mismatch");
});

test("verify detects unknown files and undeclared targets", (t) => {
  const dist = fixture(t);
  lfsTriples(dist);
  assert.equal(generate(dist).status, 0);

  writeFileSync(join(dist, "worker-manifest.sha256"), "unexpected");
  assertFailure(run(["verify", "--dist", dist, "--manifest", join(dist, "worker-manifest.json")]), "unknown_file");
  rmSync(join(dist, "worker-manifest.sha256"));

  writeBinary(dist, "x86_64-apple-darwin", "extra-macos-binary\n");
  assertFailure(run(["verify", "--dist", dist, "--manifest", join(dist, "worker-manifest.json")]), "undeclared_target");
});

test("verify detects a missing declared target and a missing required triple", (t) => {
  const dist = fixture(t);
  lfsTriples(dist);
  assert.equal(generate(dist).status, 0);

  const missing = join(dist, "x86_64-apple-darwin");
  mkdirSync(missing);
  const manifest = readManifest(dist);
  manifest.targets.push({
    triple: "x86_64-apple-darwin",
    file: `x86_64-apple-darwin/${BINARY}`,
    sha256: "1".repeat(64),
    size: 1,
  });
  manifest.targets.sort((left, right) => (left.triple < right.triple ? -1 : 1));
  writeManifest(dist, manifest);
  assertFailure(run(["verify", "--dist", dist, "--manifest", join(dist, "worker-manifest.json")]), "missing_target");

  rmSync(missing, { recursive: true });
  assertFailure(
    run([
      "verify",
      "--dist",
      dist,
      "--manifest",
      join(dist, "worker-manifest.json"),
      "--require-triple",
      "x86_64-apple-darwin",
    ]),
    "required_target_missing",
  );
});

test("verify rejects an unpinned manifest", (t) => {
  const dist = fixture(t);
  lfsTriples(dist);
  assert.equal(generate(dist).status, 0);

  const noCommit = readManifest(dist);
  delete noCommit.git_commit;
  writeManifest(dist, noCommit);
  assertFailure(run(["verify", "--dist", dist, "--manifest", join(dist, "worker-manifest.json")]), "manifest_missing_field");

  assert.equal(generate(dist).status, 0);
  const badVersion = readManifest(dist);
  badVersion.ato_version = "latest";
  writeManifest(dist, badVersion);
  assertFailure(run(["verify", "--dist", dist, "--manifest", join(dist, "worker-manifest.json")]), "unpinned_version");
});

test("verify enforces expected pin, schema, sorting and path safety", (t) => {
  const dist = fixture(t);
  lfsTriples(dist);
  assert.equal(generate(dist).status, 0);

  assertFailure(
    run(["verify", "--dist", dist, "--manifest", join(dist, "worker-manifest.json"), "--commit", OTHER_COMMIT]),
    "pin_mismatch",
  );

  const unknownField = readManifest(dist);
  unknownField.extra = true;
  writeManifest(dist, unknownField);
  assertFailure(run(["verify", "--dist", dist, "--manifest", join(dist, "worker-manifest.json")]), "manifest_unknown_field");

  assert.equal(generate(dist).status, 0);
  const unsorted = readManifest(dist);
  unsorted.targets.reverse();
  writeManifest(dist, unsorted);
  assertFailure(run(["verify", "--dist", dist, "--manifest", join(dist, "worker-manifest.json")]), "manifest_unsorted");

  assert.equal(generate(dist).status, 0);
  const traversal = readManifest(dist);
  traversal.targets[0].file = `../${BINARY}`;
  writeManifest(dist, traversal);
  assertFailure(run(["verify", "--dist", dist, "--manifest", join(dist, "worker-manifest.json")]), "manifest_invalid");

  assert.equal(generate(dist).status, 0);
  const wrongBinary = readManifest(dist);
  wrongBinary.targets[0].file = `${wrongBinary.targets[0].triple}/other-worker`;
  writeManifest(dist, wrongBinary);
  assertFailure(run(["verify", "--dist", dist, "--manifest", join(dist, "worker-manifest.json")]), "file_path_mismatch");
});

test("verify rejects an unreadable or malformed manifest", (t) => {
  const dist = fixture(t);
  lfsTriples(dist);
  assertFailure(
    run(["verify", "--dist", dist, "--manifest", join(dist, "absent.json")]),
    "manifest_missing",
  );
  writeFileSync(join(dist, "broken.json"), "{");
  assertFailure(run(["verify", "--dist", dist, "--manifest", join(dist, "broken.json")]), "manifest_invalid_json");
});

test("verify does not require the manifest to be outside the distribution directory", (t) => {
  const dist = fixture(t);
  lfsTriples(dist);
  assert.equal(generate(dist).status, 0);
  assert.ok(existsSync(join(dist, "worker-manifest.json")));
  // The manifest itself is skipped when scanning; no unknown_file false positive.
  assert.equal(run(["verify", "--dist", dist, "--manifest", join(dist, "worker-manifest.json")]).status, 0);
});

test("verify rejects a manifest for a different binary name", (t) => {
  const dist = fixture(t);
  lfsTriples(dist);
  assert.equal(generate(dist).status, 0);
  assertFailure(
    run(["verify", "--dist", dist, "--manifest", join(dist, "worker-manifest.json"), "--binary", "another-worker"]),
    "file_path_mismatch",
  );
});

test("inapplicable options are usage errors, not silent no-ops", (t) => {
  const dist = fixture(t);
  lfsTriples(dist);
  assert.equal(generate(dist).status, 0);
  assertFailure(
    run(["verify", "--dist", dist, "--manifest", join(dist, "worker-manifest.json"), "--check"]),
    "usage",
  );
  assertFailure(generate(dist, ["--manifest", "unused.json"]), "usage");
  assertFailure(run(["generate", "--dist", dist]), "usage");
});
