const S="modulepreload",w=function(e){return"/"+e},b={},A=function(t,n,o){let i=Promise.resolve();if(n&&n.length>0){let a=function(c){return Promise.all(c.map(u=>Promise.resolve(u).then(m=>({status:"fulfilled",value:m}),m=>({status:"rejected",reason:m}))))};document.getElementsByTagName("link");const l=document.querySelector("meta[property=csp-nonce]"),_=l?.nonce||l?.getAttribute("nonce");i=a(n.map(c=>{if(c=w(c),c in b)return;b[c]=!0;const u=c.endsWith(".css"),m=u?'[rel="stylesheet"]':"";if(document.querySelector(`link[href="${c}"]${m}`))return;const d=document.createElement("link");if(d.rel=u?"stylesheet":S,u||(d.as="script"),d.crossOrigin="",d.href=c,_&&d.setAttribute("nonce",_),document.head.appendChild(d),u)return new Promise((I,x)=>{d.addEventListener("load",I),d.addEventListener("error",()=>x(new Error(`Unable to preload CSS for ${c}`)))})}))}function r(a){const l=new Event("vite:preloadError",{cancelable:!0});if(l.payload=a,window.dispatchEvent(l),!l.defaultPrevented)throw a}return i.then(a=>{for(const l of a||[])l.status==="rejected"&&r(l.reason);return t().catch(r)})},R={},T="https://api.ato.run";function P(){return!!(typeof import.meta<"u"&&R?.PUBLIC_STORE_MOCK||typeof window<"u"&&/[?&]mock(\b|=1)/.test(window.location.search))}function U(e){return e.startsWith("/v1/capsules/by/")?"/mock-capsule-detail.json":e.startsWith("/v1/capsules")?"/mock-capsules.json":e}class h extends Error{status;constructor(t,n){super(t),this.name="StoreApiError",this.status=n}}async function j(e,t){const n=P()?U(e):`${T}${e}`,o=new AbortController,i=setTimeout(()=>o.abort(),1e4);try{const r=await fetch(n,{headers:{Accept:"application/json"},signal:t??o.signal});if(clearTimeout(i),!r.ok){const l=await r.text().catch(()=>"");throw new h(l||`Request failed (${r.status})`,r.status)}const a=await r.text();return a?JSON.parse(a):null}catch(r){throw clearTimeout(i),r instanceof h?r:new h(r instanceof Error?r.message:"Network error",0)}}async function H(e,t,n){try{const o=await j(`/v1/capsules/by/${encodeURIComponent(e)}/${encodeURIComponent(t)}`,n);return D(o)}catch{return null}}function s(...e){for(const t of e)if(typeof t=="string"&&t.trim())return t.trim()}function v(...e){for(const t of e){if(typeof t=="number"&&Number.isFinite(t))return t;if(typeof t=="string"){const n=Number(t.trim());if(Number.isFinite(n))return n}}}function $(...e){for(const t of e)if(typeof t=="boolean")return t}function k(e){return typeof e=="object"&&e!==null}function N(e){return k(e)?{handle:s(e.handle)??"",display_name:s(e.display_name,e.displayName)??null,verified:$(e.verified)??!1,github_id:v(e.github_id,e.githubId)??null,github_login:s(e.github_login,e.githubLogin)??null,avatar_url:s(e.avatar_url,e.avatarUrl,e.image)??null,image:s(e.image,e.avatar_url,e.avatarUrl)??null}:{handle:""}}function D(e){return{id:s(e.id)??"",slug:s(e.slug)??"",name:s(e.name)??"",description:s(e.description)??"",description_markdown:s(e.description_markdown)??null,category:s(e.category)??"",type:s(e.type)??"",downloads:v(e.downloads)??0,latest_version:s(e.latest_version,e.latestVersion)??"",publisher:N(e.publisher),releases:Array.isArray(e.releases)?e.releases.map(t=>{const n=k(t)?t:{};return{version:s(n.version)??"",size_bytes:v(n.size_bytes)??0,release_notes:s(n.release_notes)??"",created_at:s(n.created_at)??"",status:s(n.status)??""}}):[],screenshots:Array.isArray(e.screenshots)?e.screenshots.map(String):void 0,icon:s(e.icon,e.store_icon),store_icon:s(e.store_icon,e.icon),cover_image:s(e.cover_image,e.store_cover_image),store_cover_image:s(e.store_cover_image,e.cover_image),homepage:s(e.homepage)??null,repository:s(e.repository)??null,license_spdx:s(e.license_spdx)??null,created_at:s(e.created_at),updated_at:s(e.updated_at),store_review_passed:$(e.store_review_passed,e.storeReviewPassed)}}function z(e){return e>=1e6?`${(e/1e6).toFixed(1)} MB`:e>=1e3?`${(e/1e3).toFixed(1)} KB`:`${e} B`}function L(e){const t=Date.now(),n=new Date(e).getTime(),o=t-n,i=Math.floor(o/6e4);if(i<1)return"just now";if(i<60)return`${i}m ago`;const r=Math.floor(i/60);if(r<24)return`${r}h ago`;const a=Math.floor(r/24);return a<30?`${a}d ago`:`${Math.floor(a/30)}mo ago`}const F=new URLSearchParams(window.location.search),E=F.get("handle")||"",[M,...O]=E.split("/"),V=O.join("/")||M,f=document.getElementById("loading-state"),y=document.getElementById("error-state"),C=document.getElementById("hero-mount"),q=document.getElementById("detail-content"),g=document.getElementById("description-markdown"),K=document.getElementById("releases-list");async function W(){try{const e=await H(M,V);if(!e){f.classList.add("hidden"),y.classList.remove("hidden");return}f.classList.add("hidden"),C.classList.remove("hidden"),q.classList.remove("hidden"),J(e),Z(e),G(e),Q(e)}catch(e){f.classList.add("hidden"),y.classList.remove("hidden"),console.error("Failed to load capsule:",e)}}function J(e){const n={webpage:"#3b82f6",app:"#8b5cf6",webapp:"#10b981",cli:"#f59e0b",agent:"#06b6d4"}[e.type]||"#94a3b8";C.innerHTML=`
      <div class="hero">
        <div class="hero-main">
          <div class="title-row">
            ${e.icon?`<img src="${e.icon}" alt="" class="hero-icon" />`:`<div class="hero-icon-placeholder" style="background:${n}">${e.name.charAt(0).toUpperCase()}</div>`}
            <div class="title-info">
              <h1 class="hero-name">${p(e.name)}</h1>
              <div class="meta-row">
                <span class="category-badge" style="color:${n}">${e.category||e.type}</span>
                <span class="version">v${e.latest_version}</span>
              </div>
            </div>
          </div>
          <p class="hero-desc">${p(e.description)}</p>
          <div class="actions">
            <button class="btn btn-primary" id="btn-run">
              <svg width="16" height="16" viewBox="0 0 16 16" fill="none"><path d="M4 2L13 8L4 14V2Z" fill="currentColor"/></svg>
              Run
            </button>
            <button class="btn btn-secondary" id="btn-install">
              <svg width="16" height="16" viewBox="0 0 16 16" fill="none"><path d="M8 1V11M8 11L4 7M8 11L12 7M2 13H14" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>
              Install
            </button>
          </div>
        </div>
        <aside class="hero-sidebar">
          <div class="stat"><span class="stat-value">${X(e.downloads)}</span><span class="stat-label">Downloads</span></div>
          ${e.releases?.length?`<div class="stat"><span class="stat-value">${e.releases.length}</span><span class="stat-label">Releases</span></div>`:""}
          <div class="stat"><span class="stat-value">v${e.latest_version}</span><span class="stat-label">Latest</span></div>
          ${e.updated_at?`<div class="stat"><span class="stat-value">${L(e.updated_at)}</span><span class="stat-label">Updated</span></div>`:""}
          ${e.license_spdx?`<div class="stat"><span class="stat-value">${e.license_spdx}</span><span class="stat-label">License</span></div>`:""}
          <hr class="divider" />
          <div class="publisher-info">
            <div class="publisher-row">
              <div class="pub-avatar-placeholder">${(e.publisher?.handle||"?").charAt(0).toUpperCase()}</div>
              <span class="pub-name">${p(e.publisher?.handle||"Unknown")}</span>
              ${e.publisher?.verified?'<span class="verified-badge" title="Verified Publisher">✓</span>':""}
            </div>
          </div>
        </aside>
      </div>
    `;const o=E;A(()=>import("./bridge.DfTk7XcI.js"),[]).then(i=>{document.getElementById("btn-run")?.addEventListener("click",()=>i.runCapsule(o)),document.getElementById("btn-install")?.addEventListener("click",()=>i.installCapsule(o))})}function Z(e){e.description_markdown?g.innerHTML=B(e.description_markdown):g.innerHTML=`<p>${p(e.description)}</p>`}function G(e){const t=e.releases||[];if(t.length===0){document.getElementById("releases-section").classList.add("hidden");return}K.innerHTML=t.sort((n,o)=>new Date(o.created_at).getTime()-new Date(n.created_at).getTime()).slice(0,10).map(n=>`
      <div class="release">
        <div class="release-header">
          <span class="release-version">v${n.version}</span>
          <span class="release-date">${L(n.created_at)}</span>
          <span class="release-size">${z(n.size_bytes)}</span>
        </div>
        ${n.release_notes?`<div class="release-notes">${B(n.release_notes)}</div>`:""}
      </div>`).join("")}function Q(e){const t=[];if(e.homepage&&t.push({label:"Homepage",url:e.homepage}),e.repository&&t.push({label:"Repository",url:e.repository}),e.license_spdx&&t.push({label:e.license_spdx,url:"#"}),t.length>0){document.getElementById("detail-content");const n=document.createElement("div");n.className="links-section",n.innerHTML=`
        <h2 class="section-title">Links</h2>
        <div class="link-chips">
          ${t.filter(o=>o.url!=="#").map(o=>`<a href="${p(o.url)}" target="_blank" class="link-chip">${p(o.label)}</a>`).join("")}
        </div>
      `,g.after(n)}}function p(e){const t=document.createElement("div");return t.textContent=e,t.innerHTML}function B(e){return e.replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;").replace(/^### (.+)$/gm,"<h3>$1</h3>").replace(/^## (.+)$/gm,"<h2>$1</h2>").replace(/^# (.+)$/gm,"<h1>$1</h1>").replace(/```(\w*)\n([\s\S]*?)```/g,"<pre><code>$2</code></pre>").replace(/`([^`]+)`/g,"<code>$1</code>").replace(/\[([^\]]+)\]\(([^)]+)\)/g,'<a href="$2" target="_blank">$1</a>').replace(/\*\*(.+?)\*\*/g,"<strong>$1</strong>").replace(/\*(.+?)\*/g,"<em>$1</em>").replace(/\n\n/g,"</p><p>").replace(/\n/g,"<br />")}function X(e){return e>=1e6?`${(e/1e6).toFixed(1)}M`:e>=1e3?`${(e/1e3).toFixed(1)}K`:String(e)}W();
