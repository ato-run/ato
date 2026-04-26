#!/usr/bin/env node
// Build a GitHub Actions matrix from every sample's health.toml.
// Emits `matrix=<json>` on stdout (for GITHUB_OUTPUT) and the full JSON on fd 1.
//
// Matrix entry: { sample, tier, runtime, os, mode, ato_version, layer }
//
// Cost tiering (override in .github/workflows/samples-ci.yml via env):
//   - Tier 00/01: full matrix on all platforms (cheap)
//   - Tier 02: Linux full + macOS/Windows smoke (apps are expensive)
//   - Tier 03: Linux only (assertions of failure, not cross-platform)
//
// Usage:
//   node tools/build-sample-matrix.mjs [--ato-versions stable,main]

import { discoverSamples, platformsFor, LAYERS } from "./lib/discover.mjs";
import { resolve } from "node:path";

const ROOT = resolve(new URL(".", import.meta.url).pathname, "..");
const args = new Map();
for (let i = 2; i < process.argv.length; i += 2) {
  args.set(process.argv[i], process.argv[i + 1]);
}
const atoVersions = (args.get("--ato-versions") ?? "stable").split(",");

const samples = await discoverSamples(ROOT);
const include = [];

for (const s of samples) {
  const platforms = platformsFor(s);
  if (s.health.flaky?.quarantined) {
    // Quarantined samples run in a separate (informational) matrix, not the blocking one.
    continue;
  }
  for (const { os, mode } of platforms) {
    for (const ato of atoVersions) {
      // Smoke mode runs L0-L1 only; full mode runs L0-L7.
      const layers = mode === "smoke" ? LAYERS.slice(0, 2) : LAYERS;
      for (const layer of layers) {
        include.push({
          sample: s.slug,
          tier: s.tier,
          path: s.path,
          runtime: s.health.meta.runtime,
          os: osToRunner(os),
          mode,
          ato_version: ato,
          layer,
        });
      }
    }
  }
}

const matrix = { include };
const out = JSON.stringify(matrix);

// Emit to GITHUB_OUTPUT if present, else to stdout.
if (process.env.GITHUB_OUTPUT) {
  const { appendFile } = await import("node:fs/promises");
  await appendFile(process.env.GITHUB_OUTPUT, `matrix=${out}\n`);
  process.stderr.write(`Emitted ${include.length} matrix entries across ${samples.length} samples.\n`);
} else {
  process.stdout.write(out + "\n");
}

function osToRunner(os) {
  return { linux: "ubuntu-latest", macos: "macos-latest", windows: "windows-latest" }[os];
}
