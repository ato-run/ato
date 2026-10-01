// Source-owned preparation and launch share the ordinary contained Runtime.
// State, private variables, network, process group and deadline belong to its
// existing process adapter. This helper adds no authority or ambient commands.
'use strict';
const fs = require('node:fs');
const crypto = require('node:crypto');
const { spawn } = require('node:child_process');

function fail(code) {
  console.error('ATO_FORMATION_FAILURE ' + JSON.stringify({code,
    message: 'The source-owned Runtime preparation or launch could not complete'}));
  process.exitCode = 1;
}

let plan;
try {
  plan = JSON.parse(process.argv[1]);
  if (fs.statSync(plan.manifest).size > 8388608) throw Error();
  const bytes = fs.readFileSync(plan.manifest);
  if ('sha256:' + crypto.createHash('sha256').update(bytes).digest('hex') !== plan.manifest_sha256)
    throw Error();
  const scripts = JSON.parse(bytes).scripts;
  if (![...plan.setup_scripts, plan.launch_script].every(name =>
      Object.hasOwn(scripts ?? {}, name) && typeof scripts[name] === 'string')) throw Error();
} catch {
  fail('source_runtime_manifest_changed');
  process.exit(1);
}

let child;
let stopped = false;
for (const signal of ['SIGTERM', 'SIGINT']) process.on(signal, () => {
  stopped = true;
  child?.kill(signal);
});

function run(script, args) {
  if (stopped) return Promise.resolve(false);
  return new Promise(resolve => {
    child = spawn(plan.npm, ['run', script, ...args], {
      stdio: 'inherit', env: process.env, detached: false,
    });
    child.once('error', () => resolve(false));
    child.once('exit', (code, signal) => {
      child = undefined;
      resolve(code === 0 && !signal && !stopped);
    });
  });
}

(async () => {
  for (const script of plan.setup_scripts) {
    if (!await run(script, [])) {
      fail(stopped ? 'runtime_scripts_interrupted' : 'source_runtime_setup_failed');
      return;
    }
  }
  if (!await run(plan.launch_script, plan.argv.length ? ['--', ...plan.argv] : []))
    fail(stopped ? 'runtime_scripts_interrupted' : 'source_runtime_launch_failed');
})().catch(() => fail('source_runtime_scripts_failed'));
