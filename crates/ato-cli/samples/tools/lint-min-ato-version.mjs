#!/usr/bin/env node
// Enforce: `compat.min_ato_version` is immutable once set (post-v0.5 backwards-compat floor).
// Runs on every PR. If the git-tracked value of min_ato_version for a sample differs from HEAD
// (and is not a new file), fail. Initial PRs that introduce min_ato_version are allowed.
//
// Usage:
//   node tools/lint-min-ato-version.mjs            # lint all samples vs origin/main
//   node tools/lint-min-ato-version.mjs --base <ref>
import { execFile as _execFile } from "node:child_process";
import { promisify } from "node:util";
import { readFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { parse as parseToml } from "smol-toml";
import { discoverSamples } from "./lib/discover.mjs";

const execFile = promisify(_execFile);
const ROOT = resolve(new URL(".", import.meta.url).pathname, "..");
const args = new Map();
for (let i = 2; i < process.argv.length; i += 2) args.set(process.argv[i], process.argv[i + 1]);
const baseRef = args.get("--base") ?? "origin/main";

const samples = await discoverSamples(ROOT);
let failed = 0;

for (const s of samples) {
  const healthRel = join(s.path, "health.toml");
  const current = s.health.compat?.min_ato_version;
  if (!current) {
    console.error(`FAIL ${s.slug}: compat.min_ato_version is required`);
    failed++;
    continue;
  }

  let prevText;
  try {
    const { stdout } = await execFile("git", ["show", `${baseRef}:${healthRel}`], { cwd: ROOT });
    prevText = stdout;
  } catch {
    // File is new in this PR — allowed.
    console.log(`ok   ${s.slug} (new sample, min=${current})`);
    continue;
  }

  const prev = parseToml(prevText).compat?.min_ato_version;
  if (prev && prev !== current) {
    console.error(`FAIL ${s.slug}: min_ato_version changed ${prev} -> ${current} (immutable)`);
    failed++;
  } else {
    console.log(`ok   ${s.slug} (min=${current}, unchanged)`);
  }
}

process.exit(failed === 0 ? 0 : 1);
