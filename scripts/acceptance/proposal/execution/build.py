"""Build application binaries only from clean pinned execution checkout."""
import json
import pathlib
import shutil
import subprocess
from .preflight import ATO_SHA, API_SHA, approved_plan, digest, pin, write_new


def build(args):
    approved_plan(args.plan)
    pin(args.ato, ATO_SHA)
    pin(args.api, API_SHA)
    target = args.run/'target'
    # Dependency artifacts may be shared explicitly, but source cwd is ALWAYS the pin.
    command = ['cargo','build','--locked','-p','ato-formation-worker', '--bin','ato-formation-worker',
               '--example','proposal_search','--example','proposal_runtime','--target-dir',str(target)]
    subprocess.run(command, cwd=args.ato, check=True)
    package = args.run/'control-helper'
    package.mkdir()
    here = pathlib.Path(__file__).resolve().parent
    shutil.copyfile(here/'budget.rs',package/'budget.rs')
    shutil.copyfile(here/'project.rs',package/'project.rs')
    manifest = '''[package]
name="formation-d3-control"
version="0.1.0"
edition="2024"
[workspace]
[[bin]]
name="proposal-budget"
path="budget.rs"
[[bin]]
name="proposal-project"
path="project.rs"
[dependencies]
anyhow="1"
serde_json="1"
sha2="0.10"
ato-formation-worker={path=%s}
''' % json.dumps(str(args.ato/'apps/formation-worker'))
    (package/'Cargo.toml').write_text(manifest)
    shutil.copyfile(args.ato/'Cargo.lock',package/'Cargo.lock')
    # Add only this control package; dependency versions must stay on the pin's lock.
    subprocess.run(['cargo','build','--offline','--manifest-path',str(package/'Cargo.toml'),
                    '--target-dir',str(target)], check=True)
    import tomllib
    def versions(path):
        return {(p['name'],p['version'],p.get('source')) for p in tomllib.loads(path.read_text())['package']}
    allowed = versions(args.ato/'Cargo.lock') | {('formation-d3-control','0.1.0',None)}
    if not versions(package/'Cargo.lock') <= allowed:
        raise RuntimeError('control helper changed dependency versions')
    pins = {'requester':target/'debug/examples/proposal_search',
            'runtime':target/'debug/examples/proposal_runtime', 'worker':target/'debug/ato-formation-worker',
            'budget':target/'debug/proposal-budget','project':target/'debug/proposal-project'}
    result = dict(execution_sha=ATO_SHA,api_sha=API_SHA,
                  controller_helpers={name:digest((here/name).read_bytes()) for name in ['budget.rs','project.rs']},
                  execution_lock_sha256=digest((args.ato/'Cargo.lock').read_bytes()),
                  helper_lock_sha256=digest((package/'Cargo.lock').read_bytes()),
                  binaries={name:dict(path=str(path),sha256=digest(path.read_bytes())) for name,path in pins.items()})
    pin(args.ato, ATO_SHA); pin(args.api, API_SHA)
    write_new(args.binaries,result)
