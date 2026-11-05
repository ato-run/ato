import { createRequire } from 'node:module';
import { mkdir, mkdtemp, rmdir, unlink, writeFile } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { join } from 'node:path';

const require = createRequire(import.meta.url);
// esbuild is the locked dependency used by Vite; resolve it from Vite's tree.
const viteRequire = createRequire(require.resolve('vite'));
const esbuild = viteRequire('esbuild');
const workRoot = join(process.cwd(), '.tmp');
await mkdir(workRoot, { recursive: true });
const workDir = await mkdtemp(join(workRoot, 'update-security-'));
const output = join(workDir, 'test.mjs');
try {
  const built = await esbuild.build({
    entryPoints: ['tests/update-security.test.ts'], bundle: true, platform: 'node',
    format: 'esm', packages: 'external', write: false,
  });
  await writeFile(output, built.outputFiles[0].contents, { flag: 'wx' });
  const result = spawnSync(process.execPath, ['--test', output], {
    stdio: 'inherit',
    env: { ...process.env, OPENKITCHEN_SQLJS_WASM: require.resolve('sql.js/dist/sql-wasm.wasm') },
  });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exitCode = result.status ?? 1;
} finally {
  await unlink(output).catch((error) => { if (error.code !== 'ENOENT') throw error; });
  await rmdir(workDir);
}
