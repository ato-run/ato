#!/usr/bin/env python3
"""Linux Docker integration: target-only failure injection; no daemon changes.
Usage: oci-stop-unconfirmed.py ATO_BINARY CAPSULE SCRATCH
Uses each OCI derivation in CAPSULE, preserves evidence in SCRATCH, and finally
removes only the container/network IDs captured from its own test runs.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

binary, capsule, scratch = map(lambda p: str(Path(p).resolve()), sys.argv[1:4])
root = Path(scratch)
root.mkdir(parents=True, exist_ok=False)
docker = shutil.which('docker')
assert docker
bin_dir = root / 'bin'
bin_dir.mkdir()
wrapper = bin_dir / 'docker'
wrapper.write_text('''#!/bin/sh
if [ -f "$STOP_TEST_ROOT/inject" ] && [ "$1" = inspect ] && [ "$3" = '{{.State.Running}}|{{.State.ExitCode}}' ] && grep -qx "$4" "$STOP_TEST_ROOT/ids"; then
  echo 'injected target-only stop inspection failure' >&2
  exit 1
fi
if [ -f "$STOP_TEST_ROOT/partial" ] && [ "$1" = run ]; then
  previous=''
  for arg in "$@"; do
    if [ "$previous" = --name ]; then echo "$arg" > "$STOP_TEST_ROOT/partial-name"; fi
    previous="$arg"
  done
  "$STOP_TEST_DOCKER" "$@" || exit $?
  echo 'injected failure after container creation' >&2
  exit 1
fi
if [ -f "$STOP_TEST_ROOT/partial-name" ] && [ "$1" = rm ] && [ "$2" = --force ] && [ "$3" = "$(cat "$STOP_TEST_ROOT/partial-name")" ]; then
  echo 'injected failed launch removal failure' >&2
  exit 1
fi
exec "$STOP_TEST_DOCKER" "$@"
''')
wrapper.chmod(0o755)
env = dict(os.environ, PATH=f'{bin_dir}:{os.environ["PATH"]}',
           STOP_TEST_ROOT=str(root), STOP_TEST_DOCKER=docker)
partial = "--partial-start" in sys.argv[4:]
if partial:
    (root / "partial").touch()
results = []
for index, derivation in enumerate(json.loads(Path(capsule).read_bytes())['index']['derivations']):
    home = root / str(index)
    home.mkdir()
    env['ATO_HOME'] = str(home)
    def run(*args):
        return subprocess.run([binary, *args], env=env, capture_output=True,
                              text=True, timeout=180)
    imported = run('app', 'import', capsule, '--derivation', derivation)
    assert imported.returncode == 0, imported.stderr
    instance = json.loads(imported.stdout)['instance_id']
    started = run('app', 'start', instance, '--no-open')
    inspect = lambda: json.loads(run('app', 'inspect', instance).stdout)
    if partial and (root / 'partial-name').exists():
        name = (root / 'partial-name').read_text().strip()
        data = json.loads(subprocess.check_output([docker, 'inspect', name]))[0]
        try:
            assert started.returncode != 0
            active = inspect()['active_run']
            assert active['status'] == 'quarantined'
            run_root = home / 'instances' / instance / 'runs' / active['run_id']
            evidence = json.loads((run_root / 'stop-unconfirmed.json').read_bytes())
            assert f'container:{name}' in evidence['resources']
            assert (run_root / 'workspace').is_dir() and (run_root / 'oci').is_dir()
            assert not (run_root / 'stop.ack').exists()
            assert run('app', 'start', instance, '--no-open').returncode != 0
            lifecycle = json.loads((run_root / 'lifecycle.json').read_bytes())
            assert lifecycle['stop_confirmed'] is False
            results.append(dict(derivation=derivation, partial_launch_failed=True,
                                quarantined=True, scratch_kept=True, next_run_refused=True))
        finally:
            subprocess.run([docker, 'rm', '--force', name], check=True, capture_output=True)
            for network in data['NetworkSettings']['Networks']:
                subprocess.run([docker, 'network', 'rm', network], check=True, capture_output=True)
            (root / 'partial-name').unlink()
        continue
    assert started.returncode == 0, started.stderr
    before = inspect()
    active = before['active_run']
    run_root = home / 'instances' / instance / 'runs' / active['run_id']
    receipt_path = run_root / 'verification-receipt.json'
    receipt_bytes = receipt_path.read_bytes()
    receipt = json.loads(receipt_bytes)
    execution = receipt['execution']
    ids = ([execution['container_id']] if 'container_id' in execution else [])
    ids += [s['container_id'] for s in execution.get('services', [])]
    if not ids:
        assert run('app', 'stop', instance).returncode == 0
        continue
    networks = set()
    for cid in ids:
        data = json.loads(subprocess.check_output([docker, 'inspect', cid]))[0]
        networks.update(data['NetworkSettings']['Networks'])
    (root / 'ids').write_text('\n'.join(ids) + '\n')
    (root / 'inject').touch()
    try:
        stopped = run('app', 'stop', instance)
        assert stopped.returncode != 0
        after = inspect()
        assert after['active_run']['run_id'] == active['run_id']
        assert after['active_run']['status'] == 'quarantined', after
        assert receipt_path.read_bytes() == receipt_bytes
        assert receipt['fully_satisfied'] is True
        assert not (run_root / 'stop.ack').exists() or (run_root / 'stop.ack').read_text().strip() != 'ok'
        assert (run_root / 'workspace').is_dir()
        assert (run_root / 'oci').is_dir()
        lifecycle = json.loads((run_root / 'lifecycle.json').read_bytes())
        assert lifecycle['stop_confirmed'] is False
        evidence = json.loads((run_root / 'stop-unconfirmed.json').read_bytes())
        for cid in ids:
            assert f'container:{cid}' in evidence['resources']
        next_run = run('app', 'start', instance, '--no-open')
        assert next_run.returncode != 0
        # No durable state reference may advance after unconfirmed stop.
        assert before['instance']['data_snapshot_ref'] == after['instance']['data_snapshot_ref']
        results.append(dict(derivation=derivation, fully_satisfied=True,
                            quarantined=True, scratch_kept=True,
                            next_run_refused=True, stop_exit=stopped.returncode))
    finally:
        (root / 'inject').unlink(missing_ok=True)
        for cid in ids:
            subprocess.run([docker, 'rm', '--force', cid], check=True, capture_output=True)
        for network in networks:
            subprocess.run([docker, 'network', 'rm', network], check=True, capture_output=True)
(root / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
print(json.dumps(results, indent=2))
