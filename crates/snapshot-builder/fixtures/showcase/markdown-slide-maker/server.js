// Markdown Slide Maker — dependency-free Node HTTP server. Paste markdown
// (--- separates slides); render a deck, navigate with arrows, export the
// deck as a standalone HTML file. No deps, no build, no secrets. Binds 0.0.0.0.
const http = require("http");

const SAMPLE = `# Ato Snapshot v1
### Run any app in ~2 seconds

---

## The old way

- clone the repo
- npm install
- configure env
- build
- *then* run

---

## With Ato

- **Run** → the app is already ready
- restored from a sealed snapshot
- disposable, sandboxed, no setup

---

## This deck

was written in Markdown and is running
as a restored snapshot **right now**.

Press → to advance, E to export.`;

const PAGE = `<!doctype html>
<html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Markdown Slide Maker</title>
<style>
  :root{--bg:#0e1016;--panel:#161923;--line:#2a2f3e;--fg:#eef1f8;--dim:#9aa2b8;--accent:#8b5cf6}
  *{box-sizing:border-box}
  body{margin:0;font:15px/1.6 system-ui,-apple-system,Segoe UI,Roboto,sans-serif;background:var(--bg);color:var(--fg);height:100vh;display:flex;flex-direction:column}
  header{padding:14px 22px;border-bottom:1px solid var(--line);display:flex;align-items:center;gap:14px}
  header h1{font-size:16px;margin:0}
  header .tag{font-size:12px;color:var(--dim)}
  .wrap{flex:1;display:grid;grid-template-columns:340px 1fr;min-height:0}
  @media(max-width:800px){.wrap{grid-template-columns:1fr;grid-template-rows:200px 1fr}}
  .editor{border-right:1px solid var(--line);display:flex;flex-direction:column;padding:14px;gap:10px;min-height:0}
  textarea{flex:1;background:#0a0c12;color:var(--fg);border:1px solid var(--line);border-radius:10px;padding:12px;font:13px/1.55 ui-monospace,SFMono-Regular,Menlo,monospace;resize:none}
  .stage{position:relative;display:flex;align-items:center;justify-content:center;padding:24px;min-height:0}
  .slide{width:100%;max-width:820px;aspect-ratio:16/9;background:linear-gradient(160deg,#1b1f2e,#12151f);border:1px solid var(--line);border-radius:16px;padding:6% 8%;display:flex;flex-direction:column;justify-content:center;overflow:auto;box-shadow:0 20px 60px rgba(0,0,0,.35)}
  .slide h1{font-size:40px;margin:.1em 0}
  .slide h2{font-size:30px;margin:.1em 0}
  .slide h3{font-size:20px;color:var(--dim);margin:.1em 0;font-weight:600}
  .slide ul{margin:.4em 0;padding-left:1.1em}
  .slide li{margin:.28em 0}
  .slide code{background:#0a0c12;border:1px solid var(--line);border-radius:6px;padding:1px 6px;font-family:ui-monospace,Menlo,monospace}
  .nav{position:absolute;bottom:16px;display:flex;align-items:center;gap:14px;color:var(--dim);font-size:13px}
  .nav button{background:var(--panel);color:var(--fg);border:1px solid var(--line);border-radius:8px;padding:6px 12px;cursor:pointer}
  .top button{background:var(--accent);color:#0d0820;border:0;border-radius:8px;padding:8px 14px;font-weight:600;cursor:pointer}
  .top{margin-left:auto;display:flex;gap:10px}
</style></head>
<body>
<header><h1>Markdown Slide Maker</h1><span class="tag">--- separates slides · ← → navigate · E export</span>
  <span class="top"><button onclick="exportHTML()">Export HTML</button></span></header>
<div class="wrap">
  <div class="editor"><textarea id="md" spellcheck="false" oninput="render()"></textarea>
    <span style="color:var(--dim);font-size:12px" id="count"></span></div>
  <div class="stage">
    <div class="slide" id="slide"></div>
    <div class="nav"><button onclick="go(-1)">←</button><span id="pos"></span><button onclick="go(1)">→</button></div>
  </div>
</div>
<script>
const SAMPLE = ${JSON.stringify(SAMPLE)};
let slides=[], idx=0;
function mdToHtml(src){
  const esc=s=>s.replace(/[&<>]/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;'}[c]));
  const inline=s=>esc(s).replace(/\`([^\`]+)\`/g,'<code>$1</code>').replace(/\\*\\*([^*]+)\\*\\*/g,'<strong>$1</strong>').replace(/\\*([^*]+)\\*/g,'<em>$1</em>');
  const out=[]; let list=null;
  for(const raw of src.split(/\\r?\\n/)){
    const line=raw.replace(/\\s+$/,'');
    const m=line.match(/^(#{1,3})\\s+(.*)/);
    if(m){ if(list){out.push('</ul>');list=null} out.push('<h'+m[1].length+'>'+inline(m[2])+'</h'+m[1].length+'>'); continue; }
    if(/^\\s*[-*]\\s+/.test(line)){ if(!list){out.push('<ul>');list=1} out.push('<li>'+inline(line.replace(/^\\s*[-*]\\s+/,''))+'</li>'); continue; }
    if(list){out.push('</ul>');list=null}
    if(line.trim()) out.push('<p>'+inline(line)+'</p>');
  }
  if(list)out.push('</ul>');
  return out.join('');
}
function render(){
  const src=document.getElementById('md').value;
  slides=src.split(/^\\s*---\\s*$/m).map(s=>s.trim()).filter(Boolean).map(mdToHtml);
  if(!slides.length)slides=['<h2>Empty deck</h2>'];
  if(idx>=slides.length)idx=slides.length-1;
  document.getElementById('slide').innerHTML=slides[idx];
  document.getElementById('pos').textContent=(idx+1)+' / '+slides.length;
  document.getElementById('count').textContent=slides.length+' slide'+(slides.length>1?'s':'');
}
function go(d){ idx=Math.max(0,Math.min(slides.length-1,idx+d)); render(); }
function exportHTML(){
  const body=slides.map((s,i)=>'<section style="page-break-after:always;min-height:100vh;display:flex;flex-direction:column;justify-content:center;padding:8%;font-family:system-ui">'+s+'</section>').join('');
  const doc='<!doctype html><meta charset=utf-8><title>Slides</title>'+body;
  const a=document.createElement('a'); a.href='data:text/html;charset=utf-8,'+encodeURIComponent(doc);
  a.download='slides.html'; a.click();
}
addEventListener('keydown',e=>{ if(e.key==='ArrowRight')go(1); else if(e.key==='ArrowLeft')go(-1); else if(e.key.toLowerCase()==='e')exportHTML(); });
document.getElementById('md').value=SAMPLE; render();
</script>
</body></html>`;

const server = http.createServer((_req, res) => {
  res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
  res.end(PAGE);
});
server.listen(8080, "0.0.0.0", () => console.log("markdown-slide-maker on 0.0.0.0:8080"));
