#!/usr/bin/env python3
"""One fixed-policy, sequential 100-app wave using the existing Rust helper."""
import argparse
import datetime
import hashlib
import importlib.util
import json
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

sys.dont_write_bytecode = True

def digest(path):
    sha = hashlib.sha256()
    with path.open('rb') as stream:
        while chunk := stream.read(1024 * 1024):
            sha.update(chunk)
    return sha.hexdigest()

def utc():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()

def main():
    parser = argparse.ArgumentParser()
    for name in ('plan', 'sources', 'binaries', 'output', 'preregistration_commit'):
        parser.add_argument('--' + name.replace('_', '-'), required=True)
    args = parser.parse_args()
    plan_path = Path(args.plan).resolve()
    plan = json.loads(plan_path.read_text())
    assert plan['schema'] == 'ato.formation-coverage-100-plan/1'
    policy = plan['measurement_policy']
    assert policy['network'] == 'denied' and policy['model_calls'] == 0
    assert policy['candidate_producer'] == policy['decision_provider'] == 'off'
    assert policy['max_attempts_per_app'] == 4
    assert len(plan['applications']) == 100 and [app['index'] for app in plan['applications']] == list(range(1, 101))
    checkout = plan_path.parents[2]
    assert subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=checkout, text=True).strip() == args.preregistration_commit
    assert not subprocess.check_output(['git', 'status', '--porcelain'], cwd=checkout)
    assert not subprocess.check_output(['git', 'diff', '--name-only', plan['measurement_code']['ato'], 'HEAD', '--',
                                        '*.rs', 'Cargo.toml', '**/Cargo.toml', 'Cargo.lock', '**/Cargo.lock',
                                        '.cargo', 'rust-toolchain.toml', 'samples'], cwd=checkout)
    sources, binaries, output = (Path(getattr(args, name)).resolve() for name in ('sources', 'binaries', 'output'))
    helper, worker = binaries / 'coverage_baseline', binaries / 'ato-formation-worker'
    for name, expected in plan['binary_hashes'].items():
        assert digest(binaries / name) == expected, name
    for app in plan['applications']:
        archive = sources / f"{app['index']:03}.tar.gz"
        assert digest(archive) == app['archive_sha256'], app['name']
    assert shutil.disk_usage(output.parent).free >= plan['resource_policy']['minimum_free_bytes_before_measurement']
    # Reject script drift too; the committed rules/harness are part of the plan.
    here = Path(__file__).resolve().parent
    for name, expected in plan['harness_hashes'].items():
        assert digest(here / name) == expected, name
    spec = importlib.util.spec_from_file_location('classify100', here / 'classify-100.py')
    classifier = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(classifier)
    output.mkdir(exist_ok=False)
    work = output / 'workdirs'
    work.mkdir()
    meta = {'schema': 'ato.formation-coverage-100-raw/1',
            'measurement_pin': plan['measurement_code']['ato'],
            'ato_api_pin': plan['measurement_code']['ato_api'],
            'preregistration_commit': args.preregistration_commit,
            'plan_sha256': digest(plan_path), 'binary_hashes': plan['binary_hashes'],
            'harness_hashes': plan['harness_hashes'], 'started_at': utc(),
            'disk_available_bytes_at_start': shutil.disk_usage(output).free,
            'model_calls': 0, 'network': 'denied', 'applications': []}
    (output / 'start.json').write_text(json.dumps(meta, indent=2) + '\n')
    for app in plan['applications']:
        number = f"{app['index']:03}"
        app_root = work / number
        app_root.mkdir()
        temp = app_root / '.tmp'
        temp.mkdir()
        env = dict(os.environ, TMPDIR=str(temp), ATO_HOME=str(app_root / 'ato-home'))
        observation_path = output / f'{number}.json'
        command = [str(helper), str(sources / f'{number}.tar.gz'),
                   'sha256:' + app['archive_sha256'], str(app_root), str(worker), str(observation_path)]
        started_at, started = utc(), time.monotonic()
        (output / f'{number}-start.json').write_text(json.dumps({'index': app['index'], 'started_at': started_at, 'command': command}, indent=2) + '\n')
        stdout, stderr = output / f'{number}.stdout.log', output / f'{number}.stderr.log'
        with stdout.open('xb') as out, stderr.open('xb') as err:
            process = subprocess.Popen(command, env=env, stdout=out, stderr=err, start_new_session=True)
            try:
                exit_code = process.wait(timeout=policy['hard_process_timeout_seconds'])
                status = {'exit_code': exit_code}
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
                status = {'timeout_seconds': policy['hard_process_timeout_seconds'], 'exit_code': process.returncode}
        observation = json.loads(observation_path.read_text()) if observation_path.exists() else None
        if observation:
            assert observation['model_calls'] == 0
            assert observation['candidate_producer'] == observation['decision_provider'] == 'off'
            assert observation['runtime_facts'] == plan['runtime_profile']['expected_capabilities']
        journals = []
        journal_root = app_root / 'out/attempt-records'
        for path in sorted(journal_root.rglob('*.json')) if journal_root.exists() else []:
            value = json.loads(path.read_text())
            if value.get('schema') == 'ato.formation.attempt-record/1':
                journals.append(value)
        journal_path = output / f'{number}-attempt-records.json'
        journal_path.write_text(json.dumps(journals, indent=2) + '\n')
        primary, terminal, layers = classifier.classify(observation, status, journals)
        files = [stdout, stderr, journal_path]
        if observation_path.exists():
            files.append(observation_path)
        retained = []
        formed = (observation or {}).get('result', {}).get('result', {})
        for route in formed.get('verified_routes', []):
            ref = route.get('materialization_ref')
            bundle = app_root / 'out/bundles' / ref.removeprefix('sha256:')
            assert bundle.is_dir(), ref
            manifest = [{'path': str(path.relative_to(bundle)), 'sha256': digest(path), 'bytes': path.stat().st_size}
                        for path in sorted(bundle.rglob('*')) if path.is_file()]
            retained.append({'ref': ref, 'path': str(bundle), 'files': manifest})
            for filename in ('manifest.json', 'receipt.json'):
                path = bundle / filename
                if path.exists():
                    target = output / f'{number}-retained-{filename}'
                    shutil.copyfile(path, target)
                    files.append(target)
        retained_path = output / f'{number}-retained.json'
        retained_path.write_text(json.dumps(retained, indent=2) + '\n')
        files.append(retained_path)
        record = {'index': app['index'], 'name': app['name'], 'started_at': started_at,
                  'completed_at': utc(), 'elapsed_seconds': round(time.monotonic() - started, 3),
                  'process': status, 'observation': observation, 'attempt_records': journals,
                  'primary_class': primary, 'terminal_code': terminal, 'layers': layers,
                  'retained_artifacts': retained,
                  'functional_tested': False, 'infra_app_resolution': None,
                  'evidence': [{'path': path.name, 'sha256': digest(path), 'bytes': path.stat().st_size} for path in files]}
        (output / f'{number}-record.json').write_text(json.dumps(record, indent=2) + '\n')
        meta['applications'].append(record)
        (output / 'results.json').write_text(json.dumps(meta, indent=2) + '\n')
        print(json.dumps({'index': app['index'], 'name': app['name'], 'primary': primary, 'terminal': terminal,
                          'layers': ''.join(letter for letter, reached in layers.items() if reached)}), flush=True)
    meta.update(completed_at=utc(), disk_available_bytes_at_end=shutil.disk_usage(output).free)
    (output / 'results.json').write_text(json.dumps(meta, indent=2) + '\n')

if __name__ == '__main__':
    main()
