import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

// Validate the real site's link renderer without fetching or executing CDN code.
const html = readFileSync('docs/index.html', 'utf8');
const config = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)]
  .map(match => match[1]).find(source => source.includes('window.$docsify ='));
assert.ok(config, 'site configuration must be present');
const sandbox = { window: {}, URL, location: { href: 'https://ato-run.github.io/ato/', hash: '#/' } };
vm.runInNewContext(config, sandbox);
const settings = sandbox.window.$docsify;
assert.equal(settings.relativePath, true, 'nested Markdown must use its source context');

const renderer = { link: href => href };
const marked = { setOptions: () => {} };
assert.equal(settings.markdown(marked, renderer), marked);
const cases = [
  ['#/', '../README.md', 'https://github.com/ato-run/ato/blob/main/README.md'],
  ['#/concepts/application-contracts', 'computation.md', 'computation.md'],
  ['#/specifications/README', '../rfcs/accepted/COMPUTATION_ARCHITECTURE.md', '../rfcs/accepted/COMPUTATION_ARCHITECTURE.md'],
  ['#/specifications/README', '../../lib/formation/src/verify.rs', 'https://github.com/ato-run/ato/blob/main/lib/formation/src/verify.rs'],
  ['#/guides/getting-started', '../../samples/interop-static.capsule', 'https://github.com/ato-run/ato/blob/main/samples/interop-static.capsule'],
  ['#/guides/formation', '../../samples/', 'https://github.com/ato-run/ato/tree/main/samples/'],
  ['#/', 'archive/2026/documentation-reorganization/inventory.tsv', 'https://ato-run.github.io/ato/archive/2026/documentation-reorganization/inventory.tsv'],
  ['#/concepts/application-contracts', '/guides/getting-started.md', '/guides/getting-started.md'],
];
for (const [route, href, expected] of cases) {
  sandbox.location.hash = route;
  assert.equal(renderer.link(href, '', ''), expected, `${route} → ${href}`);
}
for (const path of ['docs/_sidebar.md', 'docs/rfcs/_sidebar.md']) {
  for (const [, href] of readFileSync(path, 'utf8').matchAll(/\]\(([^)]+)\)/g)) {
    assert.match(href, /^(?:\/|https:\/\/)/, `shared sidebar must have context-independent links: ${href}`);
  }
}
const readme = readFileSync('README.md', 'utf8');
assert.ok(readme.includes('ato run samples/interop-static.capsule --no-open'));
assert.ok(!readme.includes('ato pack samples/interop-static-k'));
console.log(`Documentation site acceptance: ${cases.length} route cases, shared sidebars and README example passed.`);
