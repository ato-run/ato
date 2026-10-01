// Registered generic lifecycle/rebuild operation. Runtime owns isolation,
// deadlines, network gates and resource limits; package scripts confer no grant.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const cp = require('node:child_process');
const order = ['preinstall','install','postinstall','prepublish','preprepare','prepare','postprepare'];
function failure(code, details) {
  console.error('ATO_FORMATION_FAILURE ' + JSON.stringify({code, message:JSON.stringify(details).slice(0,8192)}));
  process.exit(1);
}
function hash(file, limit=32*1024**2) {
  if (!fs.statSync(file).isFile() || fs.statSync(file).size>limit) failure('dependency_artifact_byte_limit',[]);
  return 'sha256:'+crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}
function read(file) { hash(file); return JSON.parse(fs.readFileSync(file,'utf8')); }
function run(argv, env) { cp.execFileSync(argv[0],argv.slice(1),{env,stdio:'inherit'}); }
function tools(selections) {
  const env={...process.env,CC:'/ato/unbound/gcc',CXX:'/ato/unbound/g++',MAKE:'/ato/unbound/make',
    PYTHON:'/ato/unbound/python',npm_config_python:'/ato/unbound/python',PKG_CONFIG:'/ato/unbound/pkg-config',
    RUSTC:'/ato/unbound/rustc',CARGO:'/ato/unbound/cargo'}, evidence=[], directories=[];
  for(const {name,version} of selections) {
    const dir=`/opt/ato/toolchains/${name}/${version}/bin`;
    const program=path.join(dir,name==='python'?'python3':name);
    const args=name==='gcc'?['-dumpfullversion','-dumpversion']:['--version'];
    const actual=cp.execFileSync(program,args,{encoding:'utf8',timeout:10000,maxBuffer:65536}).trim().split('\n')[0];
    const reported=['gcc','pkg-config'].includes(name)?actual:actual.split(/\s+/).at(-1);
    if([...reported.split('.'),'0','0'].slice(0,3).join('.')!==version) failure('native_toolchain_version_mismatch',[name]);
    const record={name,version,executable_sha256:hash(fs.realpathSync(program))};
    if(name==='python') {env.PYTHON=program;env.npm_config_python=program;}
    if(name==='gcc') {env.CC=program;env.CXX=path.join(dir,'g++');record.cxx_sha256=hash(fs.realpathSync(env.CXX));}
    if(name==='make') env.MAKE=program;
    if(name==='pkg-config') env.PKG_CONFIG=program;
    directories.push(dir);evidence.push(record);
  }
  env.PATH=[...directories,process.env.PATH||'/usr/bin:/bin'].join(':');
  return {env,evidence};
}
try {
  const [mode,raw]=process.argv.slice(1), plan=JSON.parse(raw);
  if(plan.schema!=='ato.npm-native-plan/1') failure('npm_lifecycle_plan_invalid',[]);
  if(process.versions.node!==plan.node_version) failure('native_toolchain_version_mismatch',['node']);
  if(hash(plan.manifest)!==plan.manifest_sha256 || hash(plan.lockfile)!==plan.lockfile_sha256) failure('proposal_source_digest_mismatch',[]);
  const npmVersion=cp.execFileSync(plan.npm,['--version'],{encoding:'utf8',timeout:10000,maxBuffer:65536}).trim();
  if(npmVersion!==plan.npm_version) failure('native_toolchain_version_mismatch',['npm']);
  const root=path.dirname(plan.manifest), manifest=read(plan.manifest), lock=read(plan.lockfile);
  if(manifest.workspaces || ![2,3].includes(lock.lockfileVersion) || !lock.packages || Object.keys(lock.packages).length>4096) failure('unsupported_npm_workspace_dependency',[]);
  const receipt=path.join(plan.receipt_directory,'completion.json');
  if(mode==='initialize') {
    fs.mkdirSync(plan.receipt_directory);
    console.log(JSON.stringify({schema:plan.schema,operation:mode}));
    process.exit(0);
  }
  const rootGyp=fs.existsSync(path.join(root,'binding.gyp')) && manifest.gypfile!==false && !manifest.scripts?.install && !manifest.scripts?.preinstall;
  const required=[], rootRequired=order.filter(name=>typeof manifest.scripts?.[name]==='string' || (name==='install' && rootGyp));
  for(const [relative,entry] of Object.entries(lock.packages)) {
    if(!relative) continue;
    if(entry.link || !relative.startsWith('node_modules/') || relative.split('/').some(p=>['.','..',''].includes(p))) failure('unsupported_npm_workspace_dependency',[relative]);
    const directory=path.join(root,relative), file=path.join(directory,'package.json');
    if(!fs.existsSync(file)) {if(entry.optional || (entry.dev && plan.production_only)) continue;failure('npm_dependency_missing',[relative]);}
    if(!fs.realpathSync(file).startsWith(fs.realpathSync(root)+path.sep)) failure('npm_dependency_path_escape',[relative]);
    const pkg=read(file), implicitGyp=fs.existsSync(path.join(directory,'binding.gyp')) && pkg.gypfile!==false && !pkg.scripts?.install && !pkg.scripts?.preinstall;
    if(entry.hasInstallScript || implicitGyp || ['preinstall','install','postinstall','prepare'].some(s=>typeof pkg.scripts?.[s]==='string')) {
      if(typeof pkg.name!=='string' || typeof pkg.version!=='string' || pkg.version!==entry.version) failure('npm_dependency_identity_mismatch',[relative]);
      required.push({name:pkg.name,version:pkg.version,path:relative,package_sha256:hash(file),integrity:entry.integrity||null,native_gyp:fs.existsSync(path.join(directory,'binding.gyp')) && pkg.gypfile!==false});
    }
  }
  if(mode==='rebuild') {
    const uncovered=required.filter(pkg=>!plan.packages.includes(pkg.name));
    const missingRoot=rootRequired.filter(name=>!plan.root_lifecycle.includes(name));
    if(uncovered.length || missingRoot.length) failure('npm_lifecycle_plan_required',{packages:uncovered.map(p=>p.name),root_lifecycle:missingRoot});
    if(plan.packages.some(name=>!required.some(p=>p.name===name)) || plan.root_lifecycle.some(name=>!rootRequired.includes(name))) failure('npm_lifecycle_plan_not_source_owned',[]);
    const {env,evidence}=tools(plan.toolchains);
    env.npm_config_nodedir=plan.node_root;env.npm_config_offline='true';env.npm_config_audit='false';env.npm_config_fund='false';
    if((rootGyp || required.some(p=>p.native_gyp)) && !['python','gcc','make'].every(name=>plan.toolchains.some(t=>t.name===name))) failure('native_toolchain_unavailable',['python','gcc','make']);
    if((rootGyp || required.some(p=>p.native_gyp)) && !fs.existsSync(path.join(plan.node_root,'include/node/node.h'))) failure('native_node_headers_unavailable',[]);
    if(plan.packages.length) run([plan.npm,'rebuild','--ignore-scripts=false','--foreground-scripts','--offline',...plan.packages],env);
    for(const script of order.filter(s=>plan.root_lifecycle.includes(s))) {
      if(script==='install' && rootGyp) run([process.execPath,path.join(plan.node_root,'lib/node_modules/npm/node_modules/node-gyp/bin/node-gyp.js'),'rebuild'],env);
      else run([plan.npm,'run',script,'--ignore-scripts'],env);
    }
    if(hash(plan.manifest)!==plan.manifest_sha256 || hash(plan.lockfile)!==plan.lockfile_sha256) failure('proposal_source_digest_mismatch',[]);
    fs.writeFileSync(receipt,JSON.stringify({schema:plan.schema,manifest_sha256:plan.manifest_sha256,lockfile_sha256:plan.lockfile_sha256,
      packages:required,root_lifecycle:rootRequired,toolchains:evidence,build_network:plan.build_network,node:process.version}),{flag:'wx'});
  } else if(mode==='audit') {
    if(required.length || rootRequired.length) {
      if(!fs.existsSync(receipt)) failure('npm_lifecycle_plan_required',{packages:required.map(p=>p.name),root_lifecycle:rootRequired});
      const completed=read(receipt);
      if(completed.schema!==plan.schema || completed.manifest_sha256!==plan.manifest_sha256 || completed.lockfile_sha256!==plan.lockfile_sha256
        || JSON.stringify(completed.packages)!==JSON.stringify(required) || JSON.stringify(completed.root_lifecycle)!==JSON.stringify(rootRequired)) failure('npm_lifecycle_completion_mismatch',[]);
    }
  } else failure('npm_lifecycle_plan_invalid',[]);
  console.log(JSON.stringify({schema:'ato.npm-native-evidence/1',operation:mode,dependency_packages:required.length,root_lifecycle:rootRequired}));
} catch(error) {
  failure('npm_native_operation_failed',[]);
}
