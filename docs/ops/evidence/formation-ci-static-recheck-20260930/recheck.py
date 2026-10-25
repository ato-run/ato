import hashlib
import json
import os
import platform
import subprocess
import tarfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent
TEST = 'tests::local_static_server_rejects_browser_state_without_the_run_token'
SPECS = [
    ('head', '06330897500092555c64cf143b241fbf93770a8d',
     '1579b0154c79b143736a9eb97afde01230ccae066f3c2a84fe6d37801e80dbe5', 3),
    ('base', 'b43eaa0c55be0229d55b8d82054d355b616d04b1',
     '5f5dce71330f5eae87cebdee41f41de2eca9c3748fd63d0cf8c295570dfd8788', 1),
]
env = dict(os.environ, CARGO_TARGET_DIR=str(ROOT / 'target'), CARGO_BUILD_JOBS='2',
           RUST_BACKTRACE='1', CARGO_TERM_COLOR='never', CARGO_NET_OFFLINE='true')
cargo = str(Path.home() / '.cargo/bin/cargo')
records = []
for label, pin, archive_hash, count in SPECS:
    archive = ROOT / (label + '-code.tar.gz')
    with tarfile.open(archive) as tar:
        members = tar.getmembers()
    with __import__('gzip').open(archive) as stream:
        assert hashlib.sha256(stream.read()).hexdigest() == archive_hash
    source = ROOT / label
    source.mkdir()
    with tarfile.open(archive) as tar:
        tar.extractall(source, filter='data')
    # Every invocation gets independent tempfile/ATO roots. Build output is
    # shared only within this dedicated task area, never with existing targets.
    for number in range(1, count + 1):
        fresh = ROOT / '.tmp' / f'{label}-{number}'
        fresh.mkdir(parents=True)
        run_env = dict(env, TMPDIR=str(fresh), ATO_HOME=str(fresh / 'ato-home'))
        command = [cargo, '+stable', 'test', '--offline', '--locked', '-p',
                   'ato-portable-application', '--lib', TEST, '--', '--exact',
                   '--nocapture', '--test-threads=1']
        log = ROOT / f'{label}-{number}.log'
        started = time.monotonic()
        print(f'start {label} {pin} run {number}', flush=True)
        with log.open('x') as output:
            process = subprocess.run(command, cwd=source, env=run_env,
                                     stdout=output, stderr=subprocess.STDOUT, timeout=1200)
        text = log.read_text()
        measured = 'running 1 test' in text and f'test {TEST} ...' in text
        record = {'label': label, 'pin': pin, 'run': number, 'command': command,
                  'exit_code': process.returncode, 'test_executed': measured,
                  'pass': measured and process.returncode == 0,
                  'elapsed_seconds': round(time.monotonic() - started, 3),
                  'log': log.name, 'log_sha256': hashlib.sha256(log.read_bytes()).hexdigest()}
        records.append(record)
        print(json.dumps(record), flush=True)
        if not measured:
            print(text[-3000:], flush=True)
            raise RuntimeError('build/infrastructure failure is not a measured test result')

result = {'schema': 'ato.formation-ci-static-recheck/1', 'test': TEST,
          'host': {'machine': platform.machine(), 'kernel': platform.release(),
                   'os_release': Path('/etc/os-release').read_text(),
                   'rustc': subprocess.check_output([str(Path.home()/'.cargo/bin/rustc'), '+stable', '--version'], text=True).strip(),
                   'cargo': subprocess.check_output([cargo, '+stable', '--version'], text=True).strip()},
          'fresh_env_root_per_invocation': True, 'dependency_network': 'offline',
          'source_changes': False, 'records': records}
(ROOT / 'results.json').write_text(json.dumps(result, indent=2) + '\n')
