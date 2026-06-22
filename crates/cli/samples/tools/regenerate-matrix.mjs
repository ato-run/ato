#!/usr/bin/env node
// Regenerate MATRIX.md from all health.toml files.
// Columns: Sample | Runtime | Difficulty | Desktop | Docker | Net | Linux | macOS | Windows | min_ato_version | Outcome
//
// Usage: node tools/regenerate-matrix.mjs

import { discoverSamples } from "./lib/discover.mjs";
import { writeFile } from "node:fs/promises";
import { resolve } from "node:path";

const ROOT = resolve(new URL(".", import.meta.url).pathname, "..");

const samples = await discoverSamples(ROOT);
samples.sort((a, b) => `${a.tier}/${a.slug}`.localeCompare(`${b.tier}/${b.slug}`));

const TIER_LABELS = {
  "00-quickstart": "Tier 00 — Quickstart",
  "01-capabilities": "Tier 01 — Capabilities",
  "02-apps": "Tier 02 — Apps",
  "03-limitations": "Tier 03 — Limitations",
};

const platformIcon = (mode) => {
  if (mode === "full") return "✅";
  if (mode === "smoke") return "🔵";
  return "—";
};

const reqIcon = (val) => (val ? "⚠️" : "—");

const outcomeIcon = (outcome) => {
  if (!outcome || outcome === "pass") return "✅";
  if (outcome === "advisory-gap") return "⚠️";
  if (outcome === "fail") return "❌";
  if (outcome === "fail-after-version") return "⏳";
  return "—";
};

const rows = samples.map((s) => {
  const p = s.health.platforms ?? {};
  const req = s.health.requires ?? {};
  const eb = s.health.expected_behavior ?? {};
  return {
    tier: s.tier,
    slug: s.slug,
    runtime:    s.health.meta?.runtime     ?? "—",
    difficulty: s.health.meta?.difficulty  ?? "—",
    desktop:    reqIcon(req.requires_desktop),
    docker:     reqIcon(req.requires_docker),
    network:    reqIcon(req.requires_network),
    linux:      platformIcon(p.linux   ?? "skip"),
    macos:      platformIcon(p.macos   ?? "skip"),
    windows:    platformIcon(p.windows ?? "skip"),
    minVer:     s.health.compat?.min_ato_version ?? "—",
    outcome:    outcomeIcon(eb.outcome),
    outcomeRaw: eb.outcome ?? "pass",
    upstreamIssue: eb.upstream_issue ?? "",
  };
});

const groups = Object.groupBy(rows, (r) => r.tier);

const lines = [
  "# Sample Matrix",
  "",
  "Auto-generated from each sample's `health.toml`. Do not edit manually — run `node tools/regenerate-matrix.mjs` to refresh.",
  "",
  "**Legend:** ✅ full/pass · 🔵 smoke · ⚠️ required/advisory-gap · ❌ expected-fail · — none/skip",
  "",
];

for (const tier of ["00-quickstart", "01-capabilities", "02-apps", "03-limitations"]) {
  const tierRows = groups[tier];
  if (!tierRows?.length) continue;
  lines.push(`## ${TIER_LABELS[tier] ?? tier}`);
  lines.push("");
  lines.push("| Sample | Runtime | Difficulty | Desktop | Docker | Net | Linux | macOS | Windows | min_ato_version | Outcome |");
  lines.push("|--------|---------|------------|---------|--------|-----|-------|-------|---------|----------------|---------|");
  for (const r of tierRows) {
    const outcomeCol = r.upstreamIssue
      ? `[${r.outcome}](${r.upstreamIssue})`
      : r.outcome;
    lines.push(
      `| [\`${r.slug}\`](${r.tier}/${r.slug}) | \`${r.runtime}\` | ${r.difficulty} | ${r.desktop} | ${r.docker} | ${r.network} | ${r.linux} | ${r.macos} | ${r.windows} | ${r.minVer} | ${outcomeCol} |`
    );
  }
  lines.push("");
}

// Advisory-gap summary section
const gaps = rows.filter((r) => r.outcomeRaw === "advisory-gap");
if (gaps.length) {
  lines.push("## Advisory Gaps");
  lines.push("");
  lines.push("Samples where ato-cli implementation diverges from spec. See [docs/SAMPLE_FINDINGS.md](docs/SAMPLE_FINDINGS.md) for details.");
  lines.push("");
  for (const g of gaps) {
    const ref = g.upstreamIssue ? ` — [upstream issue](${g.upstreamIssue})` : "";
    lines.push(`- [\`${g.slug}\`](${g.tier}/${g.slug})${ref}`);
  }
  lines.push("");
}

const content = lines.join("\n");
const outPath = resolve(ROOT, "MATRIX.md");
await writeFile(outPath, content);
console.log(`Wrote ${outPath} (${samples.length} samples)`);
