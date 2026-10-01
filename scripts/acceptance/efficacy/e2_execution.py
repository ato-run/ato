"""Shared isolated execution, reservations and stop/persist behavior for E2."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys

from efficacy.e2_fixtures import materialize
from efficacy.e2_protocol import (ROOT, PLAN, digest, require, safe_record, sha256,
                                  verify_environment, verify_registration, write_json)

HERE = ROOT / 'scripts/acceptance'
HOME = Path.home() / 'formation-foundation'
API_DIR = HOME / 'stage5b-api'
REQUESTER = HOME / 'target/debug/examples/formation_search'
RUNTIME = HOME / 'stage4/ato'
OUTPUT = HOME / 'stage4/efficacy-e2'
ORACLE_OUTPUT = HOME / 'stage4/efficacy-e2-oracle'
KEY_NAMES = ('ATO_GENERATION_JEV_API_KEY', 'ATO_DECISION_JEV_API_KEY')


def verify_fixtures(plan):
    base = ROOT / 'apps/formation-worker/fixtures/runtime-network'
    for case, fixture in plan['fixtures'].items():
        files = {str(p.relative_to(base / 'notes')): p
                 for p in (base / 'notes').rglob('*') if p.is_file()}
        files.update({p.name: p for p in (base / 'e2-holdout' / case).iterdir() if p.is_file()})
        files.update({name: base / 'e2-holdout' / name for name in ('bad.py', 'private-source.txt')})
        require({name: sha256(path) for name, path in files.items()} == fixture['files'], 'fixture hash drift: ' + case)


def preflight(plan):
    # No helper import, key read, reservation, Coordinator or Runtime before this.
    verify_registration(plan)
    verify_fixtures(plan)
    require(not any(name in os.environ for name in KEY_NAMES), 'ambient model keys forbidden; use primary stdin only')
    return verify_environment(plan, API_DIR, REQUESTER, RUNTIME)


def import_file(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def load_execution(output):
    # Pin the imported helper too; never prefer an installed stage4 alias.
    import_file('stage4_acceptance', HERE / 'formation-stage4-search.py')
    context = import_file('context_acceptance_e2', HERE / 'formation-generation-context.py')
    context.load_helpers()
    h = context.h
    require(h.API_DIR == API_DIR and h.REQ == REQUESTER and h.ATO == RUNTIME, 'helper path drift')
    h.FIX = ROOT / 'apps/formation-worker/fixtures/runtime-network'
    h.LEDGER.close()
    h.OUT = context.v1.OUT = context.OUT = output
    h.LEDGER = (output / 'ledger.jsonl').open('x')
    h.ENV = {k: v for k, v in h.ENV.items() if k not in KEY_NAMES and not k.startswith('ATO_ACCEPTANCE_')}
    return context


def execute_cell(context, output, cell, case, permutation, arm, provider, context_mode, plan, environment, key=None):
    """Reserve once, persist even setup/observation/cleanup failures, never retry."""
    write_json(output / f'{cell}.reserved', {'cell': cell, 'plan_sha256': sha256(PLAN)}, exclusive=True)
    root = output / cell
    root.mkdir()
    h, d = context.h, context.d
    identifier = cell.replace(':', '_')
    run = context.Run(cell, root, f'e2{identifier}{context.v1.RUN_ID}',
                      owner=f'e2_{identifier}_{context.v1.RUN_ID}', token=root / 'token')
    snapshot, error = {}, None
    try:
        require(preflight(plan) == environment, 'environment drift during run')
        entries, hashes = materialize(h.FIX / 'notes', h.FIX / 'e2-holdout', root / 'source', case, permutation)
        run.entries, run.fixture_sha256 = entries, digest(hashes)
        require(hashes == plan['fixtures'][case]['files'], 'fixture drift')
        require(entries == plan['fixtures'][case]['permutations'][permutation], 'mapping drift')
        parent = root / 'base.toml'
        parent.write_text((root / 'source/capsule.toml').read_text().replace('/app/app.py', '/app/bad.py'))
        d.OWNER, d.TOKEN = run.owner, run.token
        d.ensure_owner()
        h.OWNER, h.TOKEN = run.owner, run.token
        run.runtime = h.runtime(cell, 'runtime', plan['max_attempts'])
        rid = h.sql('SELECT id FROM runner_devices WHERE user_id=?', run.owner)[0]['id']
        h.wait_for(lambda: h.sql('SELECT environment_id FROM runtime_environments WHERE runtime_id=?', rid),
                   'runtime', timeout=120)
        env = dict(h.ENV)
        env.update(ATO_ACCEPTANCE_SETTLE_SECS=str(plan['harness_settle_seconds']),
                   ATO_ACCEPTANCE_EXACT_RUNTIME=rid,
                   ATO_ACCEPTANCE_GENERATION_ENTRYPOINTS=json.dumps(entries),
                   ATO_ACCEPTANCE_GENERATION_CONTEXT=context_mode,
                   ATO_ACCEPTANCE_GENERATION_PROVIDER=provider,
                   ATO_ACCEPTANCE_GENERATION_LOG=str(root / 'generation-calls.jsonl'),
                   ATO_ACCEPTANCE_GENERATION_INPUT=str(root / 'provider-request.json'),
                   ATO_GENERATION_JEV_MODEL=plan['model'])
        if arm in ('B', 'C'):
            require(bool(key), 'dedicated generation key missing')
            env['ATO_GENERATION_JEV_API_KEY'] = key
        else:
            require(key is None, 'model key supplied to zero-model cell')
        with (root / 'status.json').open('w') as stdout, (root / 'request.log').open('w') as stderr:
            run.requester = subprocess.Popen(
                [str(h.REQ), h.API, str(run.token), str(root / 'source'), str(root / 'work'), run.sid,
                 str(plan['max_attempts']), str(root / 'created.json'), str(parent)],
                env=env, stdout=stdout, stderr=stderr, start_new_session=True)
        h.wait_for(lambda: (root / 'created.json').exists() and (root / 'created.json').read_text(),
                   'created', timeout=120)
        run.satisfy = context.read_json(root / 'created.json')['satisfy_id']
        snapshot = context.settle(run)
        require(preflight(plan) == environment, 'environment drift after cell')
    except Exception as exc:
        error = type(exc).__name__ + ': ' + str(exc)
        try:
            snapshot = context.observe(run, 'error', error=error)
        except Exception as observation_error:
            snapshot['read_errors'] = [{'error': type(observation_error).__name__}]
    finally:
        try:
            context.cleanup(run)
            run.token.unlink(missing_ok=True)
        except Exception as cleanup_error:
            error = (error or '') + '; cleanup: ' + type(cleanup_error).__name__
    try:
        record = safe_record(run, snapshot, case, permutation, arm, plan, error)
    except Exception as telemetry_error:
        # Malformed telemetry must not erase the reserved cell or skip saving it.
        write_json(root / 'malformed-telemetry.json', snapshot)
        error = (error or '') + '; telemetry: ' + type(telemetry_error).__name__
        record = safe_record(run, {}, case, permutation, arm, plan, error)
    write_json(root / 'record.json', record, exclusive=True)
    return record


def check_peers(record, peers):
    for peer in peers:
        for field in ('contract_ref', 'budget', 'fixture_hash'):
            if peer.get(field) != record.get(field):
                record['violations'].append('cross_cell_' + field + '_drift')


def run_primary(plan, environment, oracle, key):
    OUTPUT.mkdir(parents=True, exist_ok=False)
    write_json(OUTPUT / 'registration.json', plan, exclusive=True)
    context = None
    results = []
    run_error = None
    try:
        context = load_execution(OUTPUT)
        context.h.start_coordinator('efficacy-e2')
        for cell in plan['cell_keys']:
            case, rest = cell.split('p')
            permutation, arm = int(rest[0]), rest[1]
            record = execute_cell(context, OUTPUT, cell, case, permutation, arm,
                                  {'A': 'deterministic_v2', 'B': 'jev_v2', 'C': 'jev_v3'}[arm],
                                  {'A': 'v3', 'B': 'v2', 'C': 'v3'}[arm], plan, environment,
                                  key if arm in ('B', 'C') else None)
            peers = [r for r in results if r['case'] == case]
            peers += [r['record'] for r in oracle['results'] if r['record']['case'] == case]
            check_peers(record, peers)
            results.append(record)
            write_json(OUTPUT / 'results.json', results)
            require(not record['violations'], 'protocol deviation; cell saved')
    except Exception as exc:
        run_error = type(exc).__name__ + ': ' + str(exc)
        raise
    finally:
        write_json(OUTPUT / 'results.json', results)
        try:
            if context is not None:
                context.h.stop_coordinator()
        except Exception as exc:
            run_error = 'Coordinator cleanup: ' + type(exc).__name__
            raise
        finally:
            write_json(OUTPUT / 'run-status.json', {
                'plan_sha256': sha256(PLAN), 'environment': environment,
                'oracle_result_sha256': digest(oracle), 'error': run_error,
                'complete': run_error is None and len(results) == plan['cells'],
            })
