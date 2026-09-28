// Local Miniflare only. SQL fault injection is a filesystem command channel,
// never a reachable admin endpoint. Production routes enforce ordinary auth.
import {Miniflare} from 'miniflare';
import {unstable_splitSqlQuery} from 'wrangler';
import {readFileSync,writeFileSync,existsSync,mkdirSync,readdirSync} from 'node:fs';
import path from 'node:path';
const bundle=path.resolve('bundle');
const modules=[{type:'ESModule',path:path.join(bundle,'worker.js')},...readdirSync(bundle).filter(f=>!f.startsWith('.')&&f.endsWith('.wasm')).map(f=>({type:'CompiledWasm',path:path.join(bundle,f)}))];
const mf=new Miniflare({modules,compatibilityDate:'2025-09-01',compatibilityFlags:['nodejs_compat'],host:'127.0.0.1',port:Number(process.env.C2_PORT??19544),d1Databases:['DB'],r2Buckets:['STORE_BUCKET'],d1Persist:'state/d1',r2Persist:'state/r2',bindings:{BETTER_AUTH_SECRET:'isolated-c2-local-only'}});
const db=await mf.getD1Database('DB');
if(!existsSync('initialized')){
 for(const f of ['schema.sql','schema-baseline.sql'])for(const sql of unstable_splitSqlQuery(readFileSync(path.join(bundle,f),'utf8')))await db.prepare(sql).run();
 writeFileSync('initialized',readFileSync(path.join(bundle,'receiver-pin.json')));
}
mkdirSync('commands',{recursive:true});let busy=false;
const timer=setInterval(async()=>{if(busy)return;busy=true;try{
 for(const file of readdirSync('commands').filter(f=>f.endsWith('.in.json'))){const out='commands/'+file.replace('.in.json','.out.json');if(existsSync(out))continue;
 try{const c=JSON.parse(readFileSync('commands/'+file));const result=await db.prepare(c.sql).bind(...(c.params??[])).all();writeFileSync(out,JSON.stringify(result));}
 catch(e){writeFileSync(out,JSON.stringify({error:String(e)}));}
 }
}finally{busy=false;}},25);
console.log('ready',String(await mf.ready));
for(const s of ['SIGTERM','SIGINT'])process.on(s,async()=>{clearInterval(timer);await mf.dispose();process.exit(0);});
