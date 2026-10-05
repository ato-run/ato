// Bundle an exact clean API pin for private, local state verification only.
// Production state authorization, dispatch, D1 and R2 are reused unchanged.
import {execFileSync} from 'node:child_process';
import {createRequire} from 'node:module';
import {mkdirSync, writeFileSync, copyFileSync, readFileSync} from 'node:fs';
import path from 'node:path';

const [apiArg, outArg, pin] = process.argv.slice(2);
if (!apiArg || !outArg || !/^[a-f0-9]{40}$/.test(pin ?? '')) throw Error('API directory, NEW output and exact pin required');
const api = path.resolve(apiArg), out = path.resolve(outArg);
if (execFileSync('git', ['rev-parse', 'HEAD'], {cwd: api, encoding: 'utf8'}).trim() !== pin) throw Error('API pin changed');
if (execFileSync('git', ['status', '--porcelain', '--untracked-files=no'], {cwd: api, encoding: 'utf8'}).trim()) throw Error('dirty API');
mkdirSync(out);
const require = createRequire(path.join(api, 'package.json'));
const {build} = require('esbuild');
const source = file => JSON.stringify(path.join(api, file));
writeFileSync(path.join(out, 'entry.ts'), `
import {Hono} from ${source('node_modules/hono/dist/index.js')};
import {runnerLeaseStateRoutes} from ${source('src/routes/runner_lease_state.ts')};
import {createDb} from ${source('src/db/index.ts')};
import {computes, computeSchemas, computeInstances, runnerDevices, runnerLeases, runs} from ${source('src/db/schema.ts')};
import {bindRunToLease, dispatchRuntimeLaunch} from ${source('src/services/runtime_launch/dispatch.ts')};
const app = new Hono();
app.route('/v1/runner-leases', runnerLeaseStateRoutes);
// Only the local filesystem control channel knows this key. The front HTTP
// listener refuses this path; this entry is never a deployment artifact.
app.post('/__acceptance/dispatch', async c => {
  if (c.req.header('x-acceptance-control') !== c.env.ACCEPTANCE_CONTROL) return c.json({error:'forbidden'}, 403);
  const b = await c.req.json();
  if (!/^state_verify_[1-9][0-9]*$/.test(b.run_id) || !/^[a-f0-9]{64}$/.test(b.runner_token_hash)
      || !/^sha256:[a-f0-9]{64}$/.test(b.materialization_ref)) return c.json({error:'fixture_invalid'}, 400);
  const db = createDb(c.env.DB), owner='state_verification_owner';
  await db.insert(computes).values({id:'cmp_state_verification', ownerUserId:owner, originKind:'github_public', originKey:'acceptance:state-verification', displayName:'State verification'}).onConflictDoNothing();
  await db.insert(computeSchemas).values({id:'csch_state_verification', computeId:'cmp_state_verification', capsuleRevisionId:'crev_state_verification', staticMaterializationId:'swm_state_verification'}).onConflictDoNothing();
  await db.insert(computeInstances).values({id:'cinst_state_verification', ownerUserId:owner, computeId:'cmp_state_verification', currentSchemaId:'csch_state_verification', hostSlug:'state-verification', status:'active'}).onConflictDoNothing();
  await db.insert(runnerDevices).values({id:'rnr_state_verification', userId:owner, displayName:'State verification', kind:'connected', tokenHash:b.runner_token_hash, status:'active', capabilitiesJson:'[]', supportedLeaseKindsJson:'["runtime_launch"]'}).onConflictDoNothing();
  await db.insert(runs).values({id:b.run_id, userId:owner, placement:'connected', status:'queued'});
  const dispatched = await dispatchRuntimeLaunch(db, {
    runId:b.run_id, computeId:'cmp_state_verification', computeSchemaId:'csch_state_verification', computeInstanceId:'cinst_state_verification',
    materializationRef:b.materialization_ref, realization:{kind:'process', argv:['/bin/true']}, publicEnv:[],
    endpoints:[], readiness:{kind:'process', timeout_ms:60000}, stateKey:'app_data', mountTarget:'/data',
  });
  if (!dispatched.ok) return c.json({error:dispatched.error}, 409);
  const leaseId='lease_'+b.run_id;
  await db.insert(runnerLeases).values({id:leaseId, runId:b.run_id, runnerId:'rnr_state_verification', status:'pending', commandJson:JSON.stringify(dispatched.value.leaseCommand), createdAt:new Date().toISOString(), updatedAt:new Date().toISOString()});
  await bindRunToLease(db, {runId:b.run_id, leaseId});
  return c.json({lease_id:leaseId, spec:dispatched.value.spec, spec_digest:dispatched.value.specDigest});
});
export default app;
`);
await build({entryPoints:[path.join(out, 'entry.ts')], outfile:path.join(out, 'worker.js'), bundle:true, format:'esm', platform:'browser', conditions:['workerd','worker','browser'], external:['node:*','crypto'],
  banner:{js:'import * as nodeCrypto from "node:crypto"; var require=name=>{if(name==="crypto")return nodeCrypto;throw new Error(`unbundled require ${name}`)};'},
  plugins:[{name:'wasm', setup(b) {b.onResolve({filter:/\.wasm$/}, args => {const input=path.resolve(args.resolveDir,args.path), name=path.basename(input);copyFileSync(input,path.join(out,name));return {path:'./'+name,external:true};});}}]});
for (const f of ['schema.sql','schema-baseline.sql','schema-baseline.json']) copyFileSync(path.join(api,'schema',f), path.join(out,f));
writeFileSync(path.join(out, 'receiver-pin.json'), JSON.stringify({sha:pin, local_functional_verification_only:true, baseline:JSON.parse(readFileSync(path.join(out,'schema-baseline.json'))).baseline_migration}));
