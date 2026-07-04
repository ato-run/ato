// CSV Insight Board — dependency-free Node HTTP server. Serves one self-
// contained page; all CSV parsing / type inference / charting runs in the
// browser. No deps, no build, no secrets, no external calls. Binds 0.0.0.0.
const http = require("http");

const SAMPLE = `date,region,product,units,revenue,returned
2026-01-03,APAC,Widget,120,3600.00,false
2026-01-04,EMEA,Gadget,,4200.50,false
2026-01-05,APAC,Widget,98,2940.00,true
2026-01-06,AMER,Gizmo,210,10500.00,false
2026-01-07,EMEA,Widget,150,4500.00,false
2026-01-08,AMER,Gadget,60,,true
2026-01-09,APAC,Gizmo,175,8750.00,false
2026-01-10,EMEA,Gizmo,,6000.00,false
2026-01-11,AMER,Widget,88,2640.00,false
2026-01-12,APAC,Gadget,143,10010.00,true`;

const PAGE = `<!doctype html>
<html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>CSV Insight Board</title>
<style>
  :root{--bg:#0f1220;--card:#171a2b;--line:#272b42;--fg:#e8eaf5;--dim:#9aa0be;--accent:#6ea8fe;--good:#4ade80;--warn:#fbbf24}
  *{box-sizing:border-box}
  body{margin:0;font:15px/1.5 system-ui,-apple-system,Segoe UI,Roboto,sans-serif;background:var(--bg);color:var(--fg)}
  header{padding:22px 28px;border-bottom:1px solid var(--line);display:flex;align-items:baseline;gap:12px}
  header h1{font-size:19px;margin:0}
  header .tag{font-size:12px;color:var(--dim)}
  main{max-width:1040px;margin:0 auto;padding:24px 20px 60px}
  .row{display:grid;grid-template-columns:1fr 1fr;gap:18px}
  @media(max-width:820px){.row{grid-template-columns:1fr}}
  .card{background:var(--card);border:1px solid var(--line);border-radius:14px;padding:16px 18px}
  .card h2{font-size:13px;text-transform:uppercase;letter-spacing:.05em;color:var(--dim);margin:0 0 12px}
  textarea{width:100%;height:150px;background:#0c0e1a;color:var(--fg);border:1px solid var(--line);border-radius:10px;padding:10px;font:13px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace;resize:vertical}
  .btns{display:flex;gap:10px;margin-top:10px;flex-wrap:wrap}
  button{background:var(--accent);color:#08122b;border:0;border-radius:9px;padding:9px 15px;font-weight:600;cursor:pointer}
  button.ghost{background:transparent;color:var(--fg);border:1px solid var(--line)}
  .stats{display:grid;grid-template-columns:repeat(3,1fr);gap:10px}
  .stat{background:#0c0e1a;border:1px solid var(--line);border-radius:10px;padding:12px}
  .stat .n{font-size:22px;font-weight:700}
  .stat .l{font-size:11px;color:var(--dim);text-transform:uppercase;letter-spacing:.04em}
  table{width:100%;border-collapse:collapse;font-size:13px;margin-top:6px}
  th,td{text-align:left;padding:7px 9px;border-bottom:1px solid var(--line);white-space:nowrap}
  th{color:var(--dim);font-weight:600}
  .type{font-size:11px;color:var(--accent)}
  .miss{color:var(--warn)}
  .scroll{overflow:auto;max-height:260px}
  svg{width:100%;height:180px;display:block}
  .bar{fill:var(--accent)}
  .axis{stroke:var(--line)}
  .lbl{fill:var(--dim);font-size:10px}
  .foot{margin-top:26px;color:var(--dim);font-size:12px;text-align:center}
</style></head>
<body>
<header><h1>CSV Insight Board</h1><span class="tag">paste or upload a CSV — schema, missing values, quick chart</span></header>
<main>
  <div class="card" style="margin-bottom:18px">
    <h2>Input</h2>
    <textarea id="csv" spellcheck="false"></textarea>
    <div class="btns">
      <button onclick="analyze()">Analyze</button>
      <label class="ghost" style="display:inline-flex;align-items:center;padding:9px 15px;border-radius:9px;border:1px solid var(--line);cursor:pointer">Upload CSV<input id="file" type="file" accept=".csv,text/csv" hidden></label>
      <button class="ghost" onclick="loadSample()">Load sample</button>
    </div>
  </div>
  <div class="stats" style="margin-bottom:18px">
    <div class="stat"><div class="n" id="s-rows">–</div><div class="l">rows</div></div>
    <div class="stat"><div class="n" id="s-cols">–</div><div class="l">columns</div></div>
    <div class="stat"><div class="n" id="s-miss">–</div><div class="l">missing cells</div></div>
  </div>
  <div class="row">
    <div class="card"><h2>Columns</h2><div class="scroll"><table id="schema"><thead><tr><th>name</th><th>type</th><th>missing</th></tr></thead><tbody></tbody></table></div></div>
    <div class="card"><h2 id="chart-title">Distribution</h2><div id="chart"></div></div>
  </div>
  <div class="card" style="margin-top:18px"><h2>First rows</h2><div class="scroll"><table id="preview"></table></div></div>
  <div class="foot">Restored from a sealed Ato snapshot — no install, no build. Everything here runs in your browser.</div>
</main>
<script>
const SAMPLE = ${JSON.stringify(SAMPLE)};
function parseCSV(text){
  const lines = text.trim().split(/\\r?\\n/).filter(l=>l.length);
  if(!lines.length) return {head:[],rows:[]};
  const split = l => { const out=[]; let cur='',q=false;
    for(const ch of l){ if(ch==='"'){q=!q} else if(ch===','&&!q){out.push(cur);cur=''} else cur+=ch } out.push(cur); return out.map(s=>s.trim()); };
  const head = split(lines[0]);
  const rows = lines.slice(1).map(split);
  return {head,rows};
}
function inferType(vals){
  const nonEmpty = vals.filter(v=>v!=='');
  if(!nonEmpty.length) return 'empty';
  const isNum = nonEmpty.every(v=>/^-?\\d+(\\.\\d+)?$/.test(v));
  if(isNum) return 'number';
  const isBool = nonEmpty.every(v=>/^(true|false)$/i.test(v));
  if(isBool) return 'boolean';
  const isDate = nonEmpty.every(v=>/^\\d{4}-\\d{2}-\\d{2}/.test(v));
  if(isDate) return 'date';
  return 'string';
}
function analyze(){
  const {head,rows} = parseCSV(document.getElementById('csv').value);
  document.getElementById('s-rows').textContent = rows.length;
  document.getElementById('s-cols').textContent = head.length;
  let missTotal=0;
  const cols = head.map((name,i)=>{
    const vals = rows.map(r=>r[i]??'');
    const miss = vals.filter(v=>v==='').length; missTotal+=miss;
    return {name,type:inferType(vals),miss,vals};
  });
  document.getElementById('s-miss').textContent = missTotal;
  const sb = document.querySelector('#schema tbody'); sb.innerHTML='';
  cols.forEach(c=>{ const tr=document.createElement('tr');
    tr.innerHTML='<td>'+esc(c.name)+'</td><td class="type">'+c.type+'</td><td'+(c.miss?' class="miss"':'')+'>'+c.miss+'</td>'; sb.appendChild(tr); });
  // preview
  const pv = document.getElementById('preview');
  pv.innerHTML='<thead><tr>'+head.map(h=>'<th>'+esc(h)+'</th>').join('')+'</tr></thead><tbody>'+
    rows.slice(0,10).map(r=>'<tr>'+head.map((_,i)=>'<td>'+esc(r[i]??'')+'</td>').join('')+'</tr>').join('')+'</tbody>';
  // chart: first numeric column, bucketed
  const num = cols.find(c=>c.type==='number');
  const chart=document.getElementById('chart'); const title=document.getElementById('chart-title');
  if(!num){ chart.innerHTML='<p style="color:var(--dim)">No numeric column to chart.</p>'; title.textContent='Distribution'; return; }
  title.textContent='Distribution — '+num.name;
  const nums = num.vals.filter(v=>v!=='').map(Number);
  const min=Math.min(...nums),max=Math.max(...nums),B=8,W=760,H=180,pad=24;
  const buckets=new Array(B).fill(0);
  nums.forEach(n=>{ let b=max===min?0:Math.floor((n-min)/(max-min)*(B-1e-9)); buckets[Math.min(B-1,b)]++; });
  const maxc=Math.max(...buckets,1), bw=(W-2*pad)/B;
  let bars='';
  buckets.forEach((c,i)=>{ const h=(H-2*pad)*(c/maxc); bars+='<rect class="bar" x="'+(pad+i*bw+3)+'" y="'+(H-pad-h)+'" width="'+(bw-6)+'" height="'+h+'" rx="3"/>'+
    '<text class="lbl" x="'+(pad+i*bw+bw/2)+'" y="'+(H-pad+13)+'" text-anchor="middle">'+(min+(max-min)*i/B).toFixed(0)+'</text>'; });
  chart.innerHTML='<svg viewBox="0 0 '+W+' '+H+'"><line class="axis" x1="'+pad+'" y1="'+(H-pad)+'" x2="'+(W-pad)+'" y2="'+(H-pad)+'"/>'+bars+'</svg>';
}
function esc(s){return String(s).replace(/[&<>]/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;'}[c]))}
function loadSample(){ document.getElementById('csv').value=SAMPLE; analyze(); }
document.getElementById('file').addEventListener('change',e=>{ const f=e.target.files[0]; if(!f)return;
  const r=new FileReader(); r.onload=()=>{document.getElementById('csv').value=r.result;analyze()}; r.readAsText(f); });
loadSample();
</script>
</body></html>`;

const server = http.createServer((_req, res) => {
  res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
  res.end(PAGE);
});
server.listen(8080, "0.0.0.0", () => console.log("csv-insight-board on 0.0.0.0:8080"));
