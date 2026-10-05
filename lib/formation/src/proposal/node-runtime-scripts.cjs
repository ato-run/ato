// Source-owned preparation and launch share the ordinary contained Runtime.
// State, private variables, network, process group and deadline belong to its
// existing process adapter. This helper adds no authority or ambient commands.
'use strict';
const fs = require('node:fs');
const crypto = require('node:crypto');
const { spawn } = require('node:child_process');

// Operational inputs come from the controlled Runtime, never from D. Freeze
// the monotonic allowance once; a wall-clock rollback cannot extend it.
const deadlineName = 'ATO_FORMATION_EXECUTION_DEADLINE_MS';
const remainingName = 'ATO_FORMATION_EXECUTION_REMAINING_MS';
const started = process.hrtime.bigint();
let deadline;
let allowance;
try {
  const absolute = process.env[deadlineName], remaining = process.env[remainingName];
  if (absolute !== undefined || remaining !== undefined) {
    if (!/^[0-9]+$/.test(absolute ?? '') || !/^[0-9]+$/.test(remaining ?? '')) throw Error();
    deadline = Number(absolute);
    const remainingMs = Number(remaining);
    if (!Number.isSafeInteger(deadline) || !Number.isSafeInteger(remainingMs)) throw Error();
    allowance = BigInt(Math.max(0, Math.min(deadline - Date.now(), remainingMs))) * 1000000n;
  }
} catch {
  fail('runtime_deadline_input_invalid');
  process.exit(1);
}
function expired() {
  return deadline !== undefined && (Date.now() >= deadline || process.hrtime.bigint() - started >= allowance);
}
// Workload scripts do not own or propagate the wrapper's control inputs.
const childEnvironment = {...process.env};
delete childEnvironment[deadlineName];
delete childEnvironment[remainingName];

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
  if (stopped || expired()) return Promise.resolve(false);
  return new Promise(resolve => {
    child = spawn(plan.npm, ['run', script, ...args], {
      stdio: 'inherit', env: childEnvironment, detached: false,
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
      fail(expired() ? 'round_deadline_exceeded' : stopped ? 'runtime_scripts_interrupted' : 'source_runtime_setup_failed');
      return;
    }
  }
  if (!await run(plan.launch_script, plan.argv.length ? ['--', ...plan.argv] : []))
    fail(expired() ? 'round_deadline_exceeded' : stopped ? 'runtime_scripts_interrupted' : 'source_runtime_launch_failed');
})().catch(() => fail('source_runtime_scripts_failed'));
