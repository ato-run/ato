import { build } from 'esbuild';
import { copyFile, mkdir } from 'node:fs/promises';

const root = new URL('./', import.meta.url);
const output = new URL('./dist/', root);
await mkdir(output, { recursive: true });
await build({
  entryPoints: [new URL('main.js', root).pathname],
  outfile: new URL('app.js', output).pathname,
  bundle: true,
  format: 'esm',
  minify: true,
  // Upstream has a Node-only dynamic fs import. Browser execution never
  // enters that branch; retain it rather than substituting another engine.
  external: ['fs'],
});
for (const name of ['index.html', 'style.css', 'capsule.toml', 'provenance.json', 'LICENSE']) {
  await copyFile(new URL(name, root), new URL(name, output));
}
await copyFile(
  new URL('node_modules/harper.js/dist/harper_wasm_bg.wasm', root),
  new URL('harper.wasm', output),
);
