// Capsule Readiness Checker — dependency-free Node HTTP server. Paste a
// capsule.toml; a browser-side checker mirrors the real Snapshot v1
// eligibility rules (docs/snapshot-v1-compatibility.md) and reports
// PASS / blockers with Ato-style messages. No deps, no build, no secrets.
const http = require("http");

const TEMPLATES = {
  "node web app": `schema_version = "0.3"
name = "my-node-app"
type = "app"
default_target = "web"

[targets.web]
runtime = "source"
driver = "node"
run = "node server.js"
port = 8080
readiness_probe = { http_get = "/" }`,
  "python (bare .py)": `schema_version = "0.3"
name = "my-python-app"
type = "app"
default_target = "web"

[targets.web]
runtime = "source"
driver = "python"
run = "app.py"
port = 8080`,
  "unsupported (secrets)": `schema_version = "0.3"
name = "needs-a-key"
type = "app"
default_target = "web"

[targets.web]
runtime = "source"
run = "node server.js"
port = 8080

[secrets.API_KEY]
required = true`,
};

const PAGE = `<!doctype html>
<html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Capsule Readiness Checker</title>
<style>
  :root{--bg:#0d1117;--card:#161b22;--line:#283040;--fg:#e6edf3;--dim:#8b95a7;--accent:#3fb950;--bad:#f85149;--warn:#d29922}
  *{box-sizing:border-box}
  body{margin:0;font:15px/1.6 system-ui,-apple-system,Segoe UI,Roboto,sans-serif;background:var(--bg);color:var(--fg)}
  header{padding:20px 26px;border-bottom:1px solid var(--line)}
  header h1{font-size:18px;margin:0}
  header p{margin:4px 0 0;color:var(--dim);font-size:13px}
  main{max-width:960px;margin:0 auto;padding:22px 18px 60px;display:grid;grid-template-columns:1fr 1fr;gap:18px}
  @media(max-width:800px){main{grid-template-columns:1fr}}
  .card{background:var(--card);border:1px solid var(--line);border-radius:14px;padding:16px 18px}
  .card h2{font-size:12px;text-transform:uppercase;letter-spacing:.05em;color:var(--dim);margin:0 0 12px}
  textarea{width:100%;height:320px;background:#0a0e14;color:var(--fg);border:1px solid var(--line);border-radius:10px;padding:12px;font:13px/1.55 ui-monospace,SFMono-Regular,Menlo,monospace;resize:vertical}
  .tmpls{display:flex;gap:8px;flex-wrap:wrap;margin-top:10px}
  .tmpls button{background:transparent;color:var(--fg);border:1px solid var(--line);border-radius:8px;padding:6px 11px;font-size:12px;cursor:pointer}
  .verdict{display:flex;align-items:center;gap:10px;font-size:17px;font-weight:700;margin-bottom:14px}
  .dot{width:12px;height:12px;border-radius:50%}
  .ok .dot{background:var(--accent)} .no .dot{background:var(--bad)}
  .ok{color:var(--accent)} .no{color:var(--bad)}
  .check{display:flex;gap:10px;padding:9px 0;border-top:1px solid var(--line);align-items:flex-start}
  .check .ic{width:18px;text-align:center;flex:0 0 18px}
  .pass .ic{color:var(--accent)} .fail .ic{color:var(--bad)} .info .ic{color:var(--dim)}
  .check .msg{font-size:13px}
  .check .msg small{color:var(--dim);display:block}
  .foot{grid-column:1/-1;color:var(--dim);font-size:12px;text-align:center;margin-top:6px}
</style></head>
<body>
<header><h1>Capsule Readiness Checker</h1><p>Paste a <code>capsule.toml</code>. This mirrors Ato's Snapshot v1 eligibility rules and reports what would seal — or why not.</p></header>
<main>
  <div class="card"><h2>capsule.toml</h2>
    <textarea id="toml" spellcheck="false" oninput="check()"></textarea>
    <div class="tmpls" id="tmpls"></div>
  </div>
  <div class="card"><h2>Verdict</h2><div id="out"></div></div>
  <div class="foot">Reference: Snapshot v1 = a sealed ready-state path for no-binding, single-process web apps. This checker runs entirely in your browser.</div>
</main>
<script>
const TEMPLATES = ${JSON.stringify(TEMPLATES)};
// A deliberately small, forgiving TOML reader — enough for capsule.toml shapes.
function readToml(src){
  const root={}, tables={}; let cur=root;
  for(const raw of src.split(/\\r?\\n/)){
    const line=raw.replace(/#.*$/,'').trim(); if(!line)continue;
    const t=line.match(/^\\[([^\\]]+)\\]$/);
    if(t){ const path=t[1].trim(); tables[path]=tables[path]||{}; cur=tables[path]; continue; }
    const kv=line.match(/^([A-Za-z0-9_.]+)\\s*=\\s*(.+)$/);
    if(kv){ let v=kv[2].trim();
      if(/^".*"$/.test(v))v=v.slice(1,-1);
      else if(v==='true')v=true; else if(v==='false')v=false;
      else if(/^-?\\d+$/.test(v))v=+v;
      cur[kv[1]]=v; }
  }
  return {root,tables};
}
function check(){
  const src=document.getElementById('toml').value;
  const {root,tables}=readToml(src);
  const out=document.getElementById('out');
  if(!src.trim()){ out.innerHTML='<p style="color:var(--dim)">Paste a capsule.toml to check.</p>'; return; }
  const checks=[];
  const P=(ok,label,detail)=>checks.push({ok,label,detail});
  // default target
  const dt=root.default_target;
  const tgtKey=dt?('targets.'+dt):Object.keys(tables).find(k=>k.startsWith('targets.'));
  const tgt=tgtKey?tables[tgtKey]:null;
  P(!!tgt, tgt?('default target resolves ('+ (tgtKey||'').replace('targets.','') +')'):'no default target', tgt?null:'declare default_target and a matching [targets.<name>]');
  // runtime
  const rt=(tgt&&(tgt.runtime||''))+''; const driver=(tgt&&(tgt.driver||''))+'';
  const rtOk = ['web','source'].includes(rt);
  const kind = rt==='web'?'static web':(driver==='node'?'node source':driver==='python'?'python source':'source (auto-detect)');
  P(rtOk, rtOk?('runtime supported — '+kind):'runtime not v1', rtOk?null:'v1 supports: static web, node source, python source');
  // port
  const port=tgt&&tgt.port;
  P(!!port, port?('port declared = '+port):'no port on the default target', port?null:'declare \`port = <n>\` on the default target');
  // run command
  const run=tgt&&tgt.run;
  const bare = typeof run==='string' && /^[^\\s]+\\.py$/.test(run.trim());
  P(!!run, run?('run command present'+(bare?' — bare .py → normalized to \`python3 '+run+'\`':'')):'no run command', run?null:'declare a run command on the default target');
  // readiness
  const rp = tables[tgtKey+'.readiness_probe']||(tgt&&tgt.readiness_probe);
  const explicit = /readiness_probe/.test(src);
  checks.push({ok:true,info:true,label: explicit?'explicit readiness probe':'readiness probe synthesized — http_get "/"', detail: explicit?null:'with only a port, Ato probes "/" (recorded as synthesized_probe=true). It must answer 200.'});
  // fail-closed shapes
  const bad=[
    [/^\\[secrets\\./m,'declares [secrets] — v1 is secret-free'],
    [/^\\[bindings\\./m,'declares [bindings] — v1 is no-binding'],
    [/^\\[external\\./m,'declares [external] — no external services in v1'],
    [/^gpu\\s*=\\s*true/m,'requires GPU — not in v1'],
  ];
  bad.forEach(([re,msg])=>{ if(re.test(src)) P(false,msg,'remove it or run this app outside Snapshot v1'); });
  // 0.0.0.0 hint (advisory)
  if(run && /127\\.0\\.0\\.1|localhost/.test(src)) checks.push({ok:true,info:true,label:'bind 0.0.0.0, not 127.0.0.1',detail:'loopback-only servers fail boot verification — the probe reaches the guest over the VM network.'});

  const fails=checks.filter(c=>c.ok===false);
  const cls=fails.length?'no':'ok';
  const verdict='<div class="verdict '+cls+'"><span class="dot"></span>'+(fails.length?'Not Snapshot v1 ready — '+fails.length+' blocker'+(fails.length>1?'s':''):'Snapshot v1 ready')+'</div>';
  const rows=checks.map(c=>{ const k=c.info?'info':(c.ok?'pass':'fail'); const ic=c.info?'•':(c.ok?'✓':'✕');
    return '<div class="check '+k+'"><span class="ic">'+ic+'</span><span class="msg">'+esc(c.label)+(c.detail?'<small>'+esc(c.detail)+'</small>':'')+'</span></div>'; }).join('');
  out.innerHTML=verdict+rows;
}
function esc(s){return String(s).replace(/[&<>]/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;'}[c]))}
const td=document.getElementById('tmpls');
Object.keys(TEMPLATES).forEach(name=>{ const b=document.createElement('button'); b.textContent=name;
  b.onclick=()=>{document.getElementById('toml').value=TEMPLATES[name];check()}; td.appendChild(b); });
document.getElementById('toml').value=TEMPLATES['node web app']; check();
</script>
</body></html>`;

const server = http.createServer((_req, res) => {
  res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
  res.end(PAGE);
});
server.listen(8080, "0.0.0.0", () => console.log("capsule-readiness-checker on 0.0.0.0:8080"));
