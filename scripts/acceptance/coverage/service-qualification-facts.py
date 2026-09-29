import tarfile,json,re,sys
old="/home/ubuntu/formation-foundation/.tmp/5c-20260929/coverage-run-1/sources/%02d.tar.gz"
new="/home/ubuntu/formation-foundation/.tmp/6b-c-20260929/new-sources/%d.tar.gz"
IDS=[int(x) for x in sys.argv[1].split(",")]
DB=re.compile(r"\b(postgres|postgis|mysql|mariadb|redis|valkey|mongo|clickhouse|elasticsearch|meilisearch|rabbitmq|minio|memcached|typesense|pgvector|qdrant|ollama)\b",re.I)
def read(t,m,limit=200000):
    try: return t.extractfile(m).read(limit).decode("utf8","ignore")
    except Exception: return ""
out={}
for i in IDS:
    t=tarfile.open((old%i) if i<=20 else (new%i))
    files={}
    for m in t.getmembers():
        parts=m.name.split("/")[1:]
        if m.isfile() and 1<=len(parts)<=3 and "node_modules" not in parts: files["/".join(parts)]=m
    root={k for k in files if "/" not in k}
    f={"markers":sorted(k for k in root if k in ("package.json","pyproject.toml","requirements.txt","setup.py","go.mod","Cargo.toml","composer.json","Gemfile","pom.xml","build.gradle","mix.exs","global.json","Procfile","capsule.toml","manage.py","artisan","Makefile") or k.endswith((".sln",".csproj"))),
       "locks":sorted(k for k in root if k in ("package-lock.json","npm-shrinkwrap.json","yarn.lock","pnpm-lock.yaml","bun.lock","bun.lockb","poetry.lock","uv.lock","Pipfile.lock","go.sum","Cargo.lock","composer.lock","Gemfile.lock","mix.lock","packages.lock.json")),
       "version_files":{}}
    for k in (".nvmrc",".node-version",".python-version",".ruby-version",".tool-versions","rust-toolchain","rust-toolchain.toml","global.json",".go-version"):
        if k in files: f["version_files"][k]=read(t,files[k],5000).strip()[:200]
    if "package.json" in root:
        try:
            p=json.loads(read(t,files["package.json"]))
            f["package_json"]={"packageManager":p.get("packageManager"),"engines":p.get("engines"),"start":(p.get("scripts") or {}).get("start"),"workspaces":bool(p.get("workspaces")),"main":p.get("main"),"bin":bool(p.get("bin"))}
        except Exception: f["package_json"]="unparseable"
    if "pyproject.toml" in root:
        x=read(t,files["pyproject.toml"])
        f["pyproject"]={"requires_python":(re.search(r'requires-python\s*=\s*"([^"]+)"',x) or [None,None])[1],"console_scripts":bool(re.search(r"^\[project\.scripts\]|^\[tool\.poetry\.scripts\]",x,re.M)),"build_backend":(re.search(r'build-backend\s*=\s*"([^"]+)"',x) or [None,None])[1]}
    if "go.mod" in root:
        x=read(t,files["go.mod"]); f["go"]={"go":(re.search(r"^go\s+(\S+)",x,re.M) or [None,None])[1],"toolchain":(re.search(r"^toolchain\s+(\S+)",x,re.M) or [None,None])[1],"main_root":"main.go" in root,"cmd_mains":sorted(k for k in files if re.match(r"cmd/[^/]+/main\.go$",k))}
    if "Cargo.toml" in root:
        x=read(t,files["Cargo.toml"]); f["rust"]={"workspace":"[workspace]" in x,"rust_version":(re.search(r'rust-version\s*=\s*"([^"]+)"',x) or [None,None])[1],"src_main":"src/main.rs" in files}
    if "composer.json" in root:
        try: c=json.loads(read(t,files["composer.json"])); f["php"]=(c.get("require") or {}).get("php")
        except Exception: f["php"]="unparseable"
    if "Gemfile" in root: f["ruby_version_in_gemfile"]=(re.search(r"^ruby\s+['\"]([^'\"]+)",read(t,files["Gemfile"]),re.M) or [None,None])[1]
    if "Procfile" in root: f["procfile"]=read(t,files["Procfile"],2000).strip().splitlines()[:6]
    dockerfiles=sorted(k for k in files if re.search(r"(^|/)Dockerfile[^/]*$",k))
    f["dockerfiles"]=dockerfiles[:6]
    ex=set();vol=set();cmd=[]
    for k in dockerfiles[:4]:
        x=read(t,files[k])
        ex|=set(re.findall(r"^\s*EXPOSE\s+([0-9 /tcpud]+)",x,re.M)); vol|=set(re.findall(r"^\s*VOLUME\s+(.+)$",x,re.M))
        cmd+= [c.strip()[:120] for c in re.findall(r"^\s*(?:CMD|ENTRYPOINT)\s+(.+)$",x,re.M)][:2]
    f["docker"]={"expose":sorted(e.strip() for e in ex),"volume":sorted(v.strip()[:80] for v in vol),"cmd":cmd[:4]}
    compose=sorted(k for k in files if re.search(r"(^|/)(docker-)?compose[^/]*\.ya?ml$",k))
    svc=set()
    for k in compose[:4]: svc|={s.lower() for s in DB.findall(read(t,files[k]))}
    f["compose"]={"files":compose[:4],"external_services":sorted(svc)}
    out[i]=f
print(json.dumps(out,indent=1))
