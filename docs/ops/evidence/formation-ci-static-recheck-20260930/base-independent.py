import hashlib
import json
import os
import subprocess
import time
from pathlib import Path

root = Path(__file__).resolve().parent
data = json.loads((root / 'results.json').read_text())
cargo = str(Path.home() / '.cargo/bin/cargo')
env = dict(os.environ, CARGO_TARGET_DIR=str(root / 'target'), CARGO_BUILD_JOBS='2',
           RUST_BACKTRACE='1', CARGO_TERM_COLOR='never', CARGO_NET_OFFLINE='true')
# The first base invocation reused Cargo's identical-code cached unit-test
# executable. Remove only this package from this task's dedicated target to
# compile its unit test from the exact base source directory independently.
with (root / 'base-package-clean.log').open('x') as output:
    subprocess.run([cargo, '+stable', 'clean', '-p', 'ato-portable-application'],
                   cwd=root / 'base', env=env, stdout=output,
                   stderr=subprocess.STDOUT, check=True)
fresh = root / '.tmp/base-2'
fresh.mkdir(parents=True)
env.update(TMPDIR=str(fresh), ATO_HOME=str(fresh / 'ato-home'))
command = [cargo, '+stable', 'test', '--offline', '--locked', '-p',
           'ato-portable-application', '--lib', data['test'], '--', '--exact',
           '--nocapture', '--test-threads=1']
log = root / 'base-2.log'
started = time.monotonic()
with log.open('x') as output:
    result = subprocess.run(command, cwd=root / 'base', env=env,
                            stdout=output, stderr=subprocess.STDOUT, timeout=1200)
text = log.read_text()
assert 'running 1 test' in text and f"test {data['test']} ..." in text
assert f"/base/apps/portable-application)" in text, text[-3000:]
record = {'label': 'base', 'pin': data['records'][-1]['pin'], 'run': 2,
          'command': command, 'exit_code': result.returncode,
          'test_executed': True, 'pass': result.returncode == 0,
          'elapsed_seconds': round(time.monotonic() - started, 3),
          'log': log.name, 'log_sha256': hashlib.sha256(log.read_bytes()).hexdigest(),
          'package_cache_cleared_before_run': True}
data['records'].append(record)
data['build_cache_note'] = 'base run 1 reused the identical-code executable; base run 2 independently compiled ato-portable-application from base after package-only clean in this dedicated target'
(root / 'results.json').write_text(json.dumps(data, indent=2) + '\n')
print(json.dumps(record), flush=True)
