// Discover all samples by walking tier directories for health.toml files.
// Returns [{ slug, tier, path, health }] where health is the parsed TOML.
import { readdir, readFile, stat } from "node:fs/promises";
import { join, relative } from "node:path";
import { parse as parseToml } from "smol-toml";

const TIERS = ["00-quickstart", "01-capabilities", "02-apps", "03-limitations"];

export async function discoverSamples(rootDir) {
  const samples = [];
  for (const tier of TIERS) {
    const tierPath = join(rootDir, tier);
    let entries;
    try {
      entries = await readdir(tierPath, { withFileTypes: true });
    } catch (e) {
      if (e.code === "ENOENT") continue;
      throw e;
    }
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      const samplePath = join(tierPath, entry.name);
      const healthPath = join(samplePath, "health.toml");
      let healthText;
      try {
        healthText = await readFile(healthPath, "utf8");
      } catch (e) {
        if (e.code === "ENOENT") {
          // Sample directory without health.toml — warn, do not fail discovery.
          console.error(`warn: ${relative(rootDir, samplePath)} has no health.toml`);
          continue;
        }
        throw e;
      }
      let health;
      try {
        health = parseToml(healthText);
      } catch (e) {
        throw new Error(`${relative(rootDir, healthPath)}: parse error: ${e.message}`);
      }
      samples.push({
        slug: entry.name,
        tier,
        path: relative(rootDir, samplePath),
        health,
      });
    }
  }
  return samples;
}

export const LAYERS = Object.freeze([
  "L0-static",
  "L1-smoke",
  "L2-functional",
  "L3-contract",
  "L4-cross-platform",
  "L5-regression",
  "L6-performance",
  "L7-compat",
]);

export function platformsFor(sample) {
  const p = sample.health.platforms ?? {};
  const out = [];
  for (const os of ["linux", "macos", "windows"]) {
    const mode = p[os] ?? "skip";
    if (mode !== "skip") out.push({ os, mode });
  }
  return out;
}
