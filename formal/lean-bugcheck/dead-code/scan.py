#!/usr/bin/env python3
"""Dead-code heuristics for the ato workspace. Usage: scan.py <repo> <cargo-check.json> <outdir>"""
import json, os, re, subprocess, sys, collections
repo, checkjson, out = sys.argv[1:4]
files = [f for f in subprocess.check_output(['git','-C',repo,'ls-files','*.rs'],text=True).split() ]
ident = re.compile(r'[A-Za-z_][A-Za-z0-9_]*')
defre = re.compile(r'^\s*pub(?:\([a-z: ]+\))?\s+(?:async\s+|const\s+|unsafe\s+)*(fn|struct|enum|trait|const|static|type)\s+([A-Za-z_][A-Za-z0-9_]*)')
total, nontest, defs = collections.Counter(), collections.Counter(), []
for f in files:
    src = open(os.path.join(repo,f),errors='replace').read()
    is_test_file = '/tests/' in f or f.startswith('tests/') or '/benches/' in f or '/examples/' in f
    cut = src.find('#[cfg(test)]\nmod ')
    body = '' if is_test_file else (src if cut < 0 else src[:cut])
    strip = lambda t: re.sub(r'//[^\n]*', '', t)
    total.update(ident.findall(strip(src))); nontest.update(ident.findall(strip(body)))
    for i, line in enumerate(body.split('\n'), 1):
        m = defre.match(line)
        if m: defs.append((f, i, m.group(1), m.group(2)))
unused = [d for d in defs if total[d[3]] == 1]
testonly = [d for d in defs if total[d[3]] > 1 and nontest[d[3]] == 1]
def dump(name, rows):
    with open(os.path.join(out,name),'w') as fh:
        for f,i,k,n in sorted(rows): fh.write(f'{f}:{i}\t{k}\t{n}\n')
dump('pub-never-referenced.txt', unused); dump('pub-test-only.txt', testonly)
# orphan modules
alltext = {f: open(os.path.join(repo,f),errors='replace').read() for f in files}
orph = []
for f in files:
    b = os.path.basename(f); stem = b[:-3]
    if b in ('main.rs','lib.rs','build.rs') or re.search(r'/(tests|benches|examples|bin)/', '/'+f): continue
    if stem == 'mod': stem = os.path.basename(os.path.dirname(f))
    crate = f.split('/src/')[0] if '/src/' in f else os.path.dirname(f)
    pat = re.compile(r'\bmod\s+(r#)?'+re.escape(stem)+r'\b|'+re.escape(b))
    if not any(pat.search(t) for g,t in alltext.items() if g != f and g.startswith(crate)): orph.append(f)
open(os.path.join(out,'orphan-modules.txt'),'w').write('\n'.join(orph)+'\n')
# allow(dead_code) census
allow = collections.Counter()
for f,t in alltext.items():
    n = len(re.findall(r'allow\((?:[a-z_, ]*\b)?(?:dead_code|unused[a-z_]*)', t))
    if n: allow['/'.join(f.split('/')[:2])] += n
# compiler warnings
warn = collections.Counter(); wl = []
for line in open(checkjson):
    try: m = json.loads(line)
    except Exception: continue
    msg = m.get('message') if m.get('reason') == 'compiler-message' else None
    if msg and msg.get('level') == 'warning' and msg.get('code'):
        sp = (msg.get('spans') or [{}])[0]
        warn[msg['code']['code']] += 1
        wl.append(f"{sp.get('file_name')}:{sp.get('line_start')}\t{msg['code']['code']}\t{msg['message']}")
open(os.path.join(out,'rustc-warnings.txt'),'w').write('\n'.join(sorted(set(wl)))+'\n')
by = lambda rows: collections.Counter('/'.join(r[0].split('/')[:2]) for r in rows).most_common(12)
print('defs scanned', len(defs)); print('never-referenced', len(unused), by(unused)); print('test-only', len(testonly), by(testonly))
print('kinds never-ref', collections.Counter(r[2] for r in unused)); print('orphans', len(orph), orph[:25])
print('allow(dead_code/unused*)', sum(allow.values()), allow.most_common(10)); print('rustc warnings', dict(warn))

# unused dependency heuristic: dependency name never appears as an identifier in the crate's sources
import glob
ud=[]
for ct in subprocess.check_output(['git','-C',repo,'ls-files','*Cargo.toml'],text=True).split():
    d=os.path.dirname(ct); txt=open(os.path.join(repo,ct)).read()
    srcs=' '.join(t for g,t in alltext.items() if g.startswith(d+'/')) if d else ''
    if not srcs: continue
    for sec in re.finditer(r'^\[((?:dev-|build-)?dependencies)\]\n(.*?)(?=^\[|\Z)', txt, re.S|re.M):
        for m in re.finditer(r'^([A-Za-z0-9_-]+)\s*(?:=|\.)', sec.group(2), re.M):
            name=m.group(1); pk=re.search(r'^'+re.escape(name)+r'\s*=.*package\s*=', sec.group(2), re.M)
            if not re.search(r'\b'+name.replace('-','_')+r'\b', srcs): ud.append(f'{ct}\t{sec.group(1)}\t{name}')
open(os.path.join(out,'deps-maybe-unused.txt'),'w').write('\n'.join(sorted(set(ud)))+'\n'); print('deps maybe unused', len(set(ud)))
