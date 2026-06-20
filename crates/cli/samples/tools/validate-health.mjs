#!/usr/bin/env node
// Validate every sample's health.toml against the JSON schema.
// Usage: node tools/validate-health.mjs [sample-path]
//   no arg = validate all samples
import { readFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { parse as parseToml } from "smol-toml";
import { discoverSamples } from "./lib/discover.mjs";

const ROOT = resolve(new URL(".", import.meta.url).pathname, "..");
const Ajv = (await import("ajv/dist/2020.js")).default;
const addFormats = (await import("ajv-formats")).default;
const schema = JSON.parse(await readFile(join(ROOT, "tools/schemas/health.schema.json"), "utf8"));
const ajv = new Ajv({ allErrors: true, strict: false });
addFormats(ajv);
const validate = ajv.compile(schema);

const target = process.argv[2];
let checks;
if (target) {
  const text = await readFile(join(resolve(target), "health.toml"), "utf8");
  checks = [{ slug: target, health: parseToml(text) }];
} else {
  checks = await discoverSamples(ROOT);
}

let failed = 0;
for (const s of checks) {
  if (!validate(s.health)) {
    failed++;
    console.error(`FAIL ${s.slug}:`);
    for (const err of validate.errors) {
      console.error(`  ${err.instancePath || "(root)"} — ${err.message}`);
    }
  } else {
    console.log(`ok   ${s.slug}`);
  }
}

process.exit(failed === 0 ? 0 : 1);
