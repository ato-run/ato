// Run from a NEW, owned directory with a pinned bundle and node_modules link.
// D1/R2 remain local. Production lease-state and Formation input routes share
// this explicitly isolated Coordinator; the private dispatch route stays hidden.
import http from 'node:http';
import {randomBytes} from 'node:crypto';
import {Miniflare} from 'miniflare';
import {unstable_splitSqlQuery} from 'wrangler';
import {readFileSync, writeFileSync, existsSync, mkdirSync, readdirSync} from 'node:fs';
import path from 'node:path';

const port = Number(process.env.C2_PORT);
if (!Number.isInteger(port) || port < 1024 || port > 65535) throw Error('explicit local front port required');
if (existsSync('initialized')) throw Error('use a fresh acceptance directory; preserve earlier evidence');
const control = randomBytes(32).toString('hex'), bundle = path.resolve('bundle');
const modules = [{type:'ESModule', path:path.join(bundle,'worker.js')}, ...readdirSync(bundle).filter(f=>!f.startsWith('.') && f.endsWith('.wasm')).map(f=>({type:'CompiledWasm',path:path.join(bundle,f)}))];
const mf = new Miniflare({modules, compatibilityDate:'2025-09-01', compatibilityFlags:['nodejs_compat'], host:'127.0.0.1', port:0,
  d1Databases:['DB'], r2Buckets:['STORE_BUCKET'], d1Persist:'state/d1', r2Persist:'state/r2',
  bindings:{ACCEPTANCE_CONTROL:control, BETTER_AUTH_SECRET:randomBytes(32).toString('hex'), AI_KEYS_MASTER_SECRET:randomBytes(32).toString('hex')}});
const db = await mf.getD1Database('DB');
for (const f of ['schema.sql','schema-baseline.sql']) for (const sql of unstable_splitSqlQuery(readFileSync(path.join(bundle,f),'utf8'))) await db.prepare(sql).run();
writeFileSync('initialized', readFileSync(path.join(bundle,'receiver-pin.json')), {flag:'wx',mode:0o600});
mkdirSync('commands', {mode:0o700});
let busy = false;
const timer = setInterval(async () => {
  if (busy) return;
  busy = true;
  try {
    for (const file of readdirSync('commands').filter(f=>/^[a-f0-9]+\.in\.json$/.test(f))) {
      const out=path.join('commands',file.replace('.in.json','.out.json'));
      if (existsSync(out)) continue;
      let result;
      try {
        const c=JSON.parse(readFileSync(path.join('commands',file),'utf8'));
        if (c.operation === 'dispatch') {
          const response=await mf.dispatchFetch('http://acceptance.control.invalid/__acceptance/dispatch', {method:'POST',headers:{'x-acceptance-control':control,'content-type':'application/json'},body:JSON.stringify(c.input)});
          result={status:response.status,body:await response.json()};
        } else if (c.operation === 'sql' || (c.operation === undefined && typeof c.sql === 'string')) result=await db.prepare(c.sql).bind(...(c.params??[])).all();
        else throw Error('unknown local command');
      } catch {result={error:'local command failed'};}
      writeFileSync(out,JSON.stringify(result),{flag:'wx',mode:0o600});
    }
  } finally {busy=false;}
},25);
const server=http.createServer(async (request,response) => {
  if (!/^\/v1\/runner-leases\/[a-zA-Z0-9_-]+\/state\//.test(request.url ?? '') && !/^\/v1\/runtime-network\//.test(request.url ?? '')) {response.writeHead(404);response.end();return;}
  try {
    const chunks=[];
    let size=0;
    for await (const chunk of request) {size+=chunk.length;if(size>64*1024*1024)throw Error('bounded state request');chunks.push(chunk);}
    const incoming=await mf.dispatchFetch('http://state.acceptance.invalid'+request.url,{method:request.method,headers:request.headers,
      ...(['GET','HEAD'].includes(request.method)?{}:{body:Buffer.concat(chunks)})});
    response.writeHead(incoming.status,Object.fromEntries(incoming.headers));
    response.end(Buffer.from(await incoming.arrayBuffer()));
  } catch {response.writeHead(503);response.end();}
});
await new Promise(resolve=>server.listen(port,'127.0.0.1',resolve));
console.log(JSON.stringify({ready:true,port,API_pin:JSON.parse(readFileSync('initialized')).sha,remote_migration:false,deployed:false}));
for (const signal of ['SIGINT','SIGTERM']) process.on(signal,async()=>{clearInterval(timer);server.close();await mf.dispose();process.exit(0);});
