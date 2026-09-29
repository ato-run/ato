// Bundle only the production routes from the exact, already merged C1 checkout.
// No auth/Coordinator/validator mocking, deployment, or remote database access.
import {execFileSync} from 'node:child_process';
import {createRequire} from 'node:module';
import {mkdirSync,writeFileSync,copyFileSync,readFileSync} from 'node:fs';
import path from 'node:path';
const [apiArg,outArg,requestedPin]=process.argv.slice(2);
const api=path.resolve(apiArg),out=path.resolve(outArg);
const pin=requestedPin??'b81ef143caac3f8e478b0954e9a5db2932c99f19';
if(!['b81ef143caac3f8e478b0954e9a5db2932c99f19','38668a97e7632256b33074b0d223670c16b76bfd','0eb8eb5caaa24d3889cea5481e6005d3bb8b603d','21e7e0e0b4bf82949278340d4dad7daa8116cf86'].includes(pin))throw Error('unregistered receiver pin');
if(execFileSync('git',['rev-parse','HEAD'],{cwd:api,encoding:'utf8'}).trim()!==pin)throw Error('C1 pin mismatch');
if(execFileSync('git',['status','--porcelain','--untracked-files=no'],{cwd:api,encoding:'utf8'}).trim())throw Error('dirty receiver');
const require=createRequire(path.join(api,'package.json'));const {build}=require('esbuild');
mkdirSync(out,{recursive:true});
writeFileSync(path.join(out,'entry.ts'),`import {Hono} from ${JSON.stringify(path.join(api,'node_modules/hono/dist/index.js'))};\nimport {runtimeNetworkRoutes} from ${JSON.stringify(path.join(api,'src/routes/runtime_network.ts'))};\nconst app=new Hono();app.route('/v1/runtime-network',runtimeNetworkRoutes);export default app;\n`);
await build({entryPoints:[path.join(out,'entry.ts')],outfile:path.join(out,'worker.js'),bundle:true,format:'esm',platform:'browser',conditions:['workerd','worker','browser'],external:['node:*','crypto'],
 banner:{js:'import * as nodeCrypto from "node:crypto"; var require=name=>{if(name==="crypto")return nodeCrypto;throw new Error(`unbundled require ${name}`)};'},
 plugins:[{name:'wasm',setup(b){b.onResolve({filter:/\.wasm$/},args=>{const input=path.resolve(args.resolveDir,args.path);const name=path.basename(input);copyFileSync(input,path.join(out,name));return{path:'./'+name,external:true};});}}]});
for(const f of ['schema.sql','schema-baseline.sql','schema-baseline.json'])copyFileSync(path.join(api,'schema',f),path.join(out,f));
copyFileSync(path.join(api,'src/services/runtime_network/wasm/provenance.json'),path.join(out,'authority-provenance.json'));
writeFileSync(path.join(out,'receiver-pin.json'),JSON.stringify({sha:pin,baseline:JSON.parse(readFileSync(path.join(out,'schema-baseline.json'))).baseline_migration}));
