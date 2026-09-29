#!/usr/bin/env python3
"""Execute the immutable cohort through existing local Formation, no AI paths."""
import concurrent.futures,hashlib,json,pathlib,subprocess,sys,urllib.request
PLAN_HASH='09dd673a79b73e561dc872e75268008c4703615097a1019495ed652e51e7f556'
def digest(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def main():
    plan_path,root_arg,bin_arg=sys.argv[1:]
    plan_path=pathlib.Path(plan_path);assert digest(plan_path)==PLAN_HASH
    plan=json.loads(plan_path.read_text());root=pathlib.Path(root_arg).resolve();root.mkdir(parents=True,exist_ok=False)
    binaries=pathlib.Path(bin_arg).resolve();(root/'sources').mkdir();(root/'results').mkdir();(root/'scratch').mkdir()
    (root/'binary-hashes.json').write_text(json.dumps({n:digest(binaries/n) for n in ['coverage_baseline','ato-formation-worker']},indent=2))
    def acquire(app):
        path=root/'sources'/f"{app['index']:02}.tar.gz"
        with urllib.request.urlopen(app['archive_url'],timeout=120) as response,path.open('xb') as f:
            size=0
            while data:=response.read(1024*1024):
                size+=len(data);assert size<=app['archive_bytes'];f.write(data)
        assert digest(path)==app['archive_sha256'],app['name']
        return path
    with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
        paths=list(pool.map(acquire,plan['applications']))
    # All cohort hashes confirmed before the first Formation invocation.
    for app,path in zip(plan['applications'],paths):
        output=root/'results'/f"{app['index']:02}.json"
        import os
        env=dict(os.environ);env['TMPDIR']=str(root/'scratch')
        with (root/'results'/f"{app['index']:02}.log").open('w') as log:
            completed=subprocess.run([binaries/'coverage_baseline',path,'sha256:'+app['archive_sha256'],root/'scratch'/str(app['index']),binaries/'ato-formation-worker',output],env=env,stdout=log,stderr=subprocess.STDOUT,timeout=360)
        assert completed.returncode==0,app['name']
        observation=json.loads(output.read_text());assert observation['model_calls']==0
        print(json.dumps({'app':app['name'],'result':observation['result'],'sha256':digest(output)}),flush=True)
if __name__=='__main__':main()
