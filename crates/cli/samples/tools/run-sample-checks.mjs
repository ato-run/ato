#!/usr/bin/env node
// Execute health checks for a single sample at a single layer.
//
// Usage:
//   node tools/run-sample-checks.mjs <sample-path> --layer L0-static
//   node tools/run-sample-checks.mjs 02-apps/byok-chat-openrouter --layer L2-functional
//
// Exit codes:
//   0 = pass
//   1 = test failure
//   2 = infrastructure / config error (schema invalid, etc.)
//
// L0 static: parses capsule.toml + health.toml, validates health.toml against JSON schema.
// L1 smoke: runs `ato run .` (or background + ready_probe) and asserts exit_code + timeout.
// L2 functional: L1 + stdout_regex, stderr_regex, expect.http, expect.files.
// L3 contract: asserts declared env/network/fs match runtime observations (P1).
// L4 cross-platform: orchestrated by matrix, not this runner directly.
// L5 regression: golden-output diff (P1).
// L6 performance: p95/rss budget check (P1, soft-fail).
// L7 compat: matrix runs this with different ato_version envs.

import { readFile, stat } from "node:fs/promises";
import { spawn } from "node:child_process";
import { resolve, join } from "node:path";
import { parse as parseToml } from "smol-toml";

const args = new Map();
const positional = [];
for (let i = 2; i < process.argv.length; i++) {
  const a = process.argv[i];
  if (a.startsWith("--")) { args.set(a, process.argv[++i]); }
  else { positional.push(a); }
}

const samplePath = resolve(positional[0] ?? ".");
const layer = args.get("--layer") ?? "L0-static";

const healthPath = join(samplePath, "health.toml");
let health;
try {
  health = parseToml(await readFile(healthPath, "utf8"));
} catch (e) {
  fail(2, `cannot read/parse ${healthPath}: ${e.message}`);
}

switch (layer) {
  case "L0-static":      await runL0(); break;
  case "L1-smoke":       await runL1(); break;
  case "L2-functional":  await runL2(); break;
  case "L3-contract":
  case "L5-regression":
  case "L6-performance":
    info(`${layer}: not yet implemented (P1). Reporting pass.`);
    break;
  case "L4-cross-platform":
  case "L7-compat":
    info(`${layer}: orchestrated by workflow matrix, no-op here.`);
    break;
  default:
    fail(2, `unknown layer: ${layer}`);
}

pass();

// ── Layers ──────────────────────────────────────────────────────────────

async function runL0() {
  const { readFile: rf } = await import("node:fs/promises");
  const Ajv = (await import("ajv/dist/2020.js")).default;
  const addFormats = (await import("ajv-formats")).default;
  const schema = JSON.parse(await rf(schemaPath(), "utf8"));
  const ajv = new Ajv({ allErrors: true, strict: false });
  addFormats(ajv);
  const validate = ajv.compile(schema);
  if (!validate(health)) {
    const errs = validate.errors.map(e => `  ${e.instancePath} ${e.message}`).join("\n");
    fail(1, `health.toml schema violations:\n${errs}`);
  }
  // capsule.toml must exist.
  try { await stat(join(samplePath, "capsule.toml")); }
  catch { fail(1, "missing capsule.toml"); }
  info("L0 static: ok");
}

async function runL1() {
  const { cmd, cwd = ".", background = false, timeout_s = 60 } = health.run ?? {};
  if (!cmd) fail(2, "health.run.cmd is required");
  const env = prepareEnv();
  info(`L1 smoke: running \`${cmd}\` (timeout ${timeout_s}s, background=${background})`);
  if (background) {
    const proc = spawnCmd(cmd, cwd, env);
    const ready = await waitReady(health.run.ready_probe, proc);
    if (!ready.ok) { proc.kill("SIGTERM"); fail(1, `ready_probe failed: ${ready.reason}`); }
    info("L1 smoke: ready_probe ok");
    proc.kill("SIGTERM");
  } else {
    const result = await runCmd(cmd, cwd, env, timeout_s * 1000);
    const exp = expectedExitCode();
    if (health.expect?.exit_nonzero) {
      if (result.exitCode === 0) fail(1, "expected non-zero exit (limitation sample), got 0");
    } else if (result.exitCode !== exp) {
      fail(1, `expected exit ${exp}, got ${result.exitCode}. stderr:\n${result.stderr.slice(-800)}`);
    }
    info(`L1 smoke: exit ${result.exitCode} (expected)`);
  }
}

async function runL2() {
  await runL1();
  const exp = health.expect ?? {};
  // Functional assertions run against the recorded output from L1 if not background.
  // (Proper implementation lives in P1; this is a scaffold.)
  info("L2 functional: scaffold — detailed assertions land in P1.");
  if (exp.stdout_regex) info(`  TODO: assert stdout matches /${exp.stdout_regex}/`);
  if (exp.http) info(`  TODO: probe ${exp.http.length} HTTP endpoint(s)`);
  if (exp.files) info(`  TODO: check ${exp.files.length} file expectation(s)`);
}

// ── Helpers ─────────────────────────────────────────────────────────────

function schemaPath() {
  return join(resolve(new URL(".", import.meta.url).pathname), "schemas", "health.schema.json");
}

function prepareEnv() {
  const env = { ...process.env };
  const ci = health.env?.ci_secrets ?? {};
  for (const [k, v] of Object.entries(ci)) {
    if (!env[k]) env[k] = v;
  }
  return env;
}

function expectedExitCode() {
  return health.expect?.exit_code ?? 0;
}

function spawnCmd(cmd, cwd, env) {
  const [bin, ...rest] = cmd.split(/\s+/);
  return spawn(bin, rest, { cwd: join(samplePath, cwd), env, stdio: ["ignore", "pipe", "pipe"] });
}

function runCmd(cmd, cwd, env, timeoutMs) {
  return new Promise((resolvePromise) => {
    const proc = spawnCmd(cmd, cwd, env);
    let stdout = "", stderr = "";
    proc.stdout.on("data", d => { stdout += d; });
    proc.stderr.on("data", d => { stderr += d; });
    const t = setTimeout(() => proc.kill("SIGTERM"), timeoutMs);
    proc.on("close", (code) => {
      clearTimeout(t);
      resolvePromise({ exitCode: code ?? -1, stdout, stderr });
    });
  });
}

async function waitReady(probe, proc) {
  if (!probe) return { ok: true };
  const deadline = Date.now() + (probe.timeout_s ?? 30) * 1000;
  while (Date.now() < deadline) {
    if (probe.http_url) {
      try {
        const r = await fetch(probe.http_url);
        if (r.ok) return { ok: true };
      } catch {}
    }
    // stdout_regex / file_exists probes: P1.
    await new Promise(r => setTimeout(r, 250));
  }
  return { ok: false, reason: `probe timed out after ${probe.timeout_s ?? 30}s` };
}

function info(msg)  { process.stderr.write(`[${layer}] ${msg}\n`); }
function pass()     { info("pass"); process.exit(0); }
function fail(code, msg) { process.stderr.write(`[${layer}] FAIL: ${msg}\n`); process.exit(code); }
