#!/usr/bin/env python3
"""Fixed 6b-F two-phase measurement; production Rust retains every authority.

This controller has no model transport or credential reader. The known phase
uses the unchanged local baseline helper. Eligible proposal phases use the
unchanged Requester/Coordinator/Runtime APIs through measurement-only glue.
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import subprocess
import time
import sys

sys.dont_write_bytecode = True

def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()

def read(path):
    return json.loads(Path(path).read_text())

def write(path, value):
    with Path(path).open('x') as stream:
        json.dump(value, stream, indent=2); stream.write('\n'); stream.flush(); os.fsync(stream.fileno())

def utc():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()

def require(value, message):
    if not value:
        raise RuntimeError(message)

def adaptive_terminal(observed, calls):
    status = observed['status']; round_ = status.get('proposal_round') or {}
    if observed['accepted']: return 'proposal_pass'
    if status.get('termination_reason') == 'decision_stopped': return 'proposal_declined'
    error = (round_.get('provider_call') or {}).get('error_class')
    if error == 'budget_exhausted': return 'budget_exhausted'
    if round_.get('status') == 'timeout': return 'provider_timeout'
    if round_.get('error_class') == 'invalid_output': return 'proposal_schema'
    if round_.get('status') == 'provider_error': return 'provider_error'
    outcomes = round_.get('outcomes') or []
    if any(x.get('status') == 'rejected' and 'unauthorized' in x.get('code', '') for x in outcomes):
        return 'proposal_id_unauthorized'
    admitted = [x for x in outcomes if x.get('status') == 'admitted']
    if not admitted:
        if outcomes and all(x.get('status') == 'unsupported' for x in outcomes): return 'proposal_declined'
        return 'proposal_invalid' if outcomes else 'producer_not_invoked' if not calls else 'proposal_declined'
    codes = {x.get('failure_code') for x in (status.get('search_state') or {}).get('attempts', [])}
    if 'network_denied' in codes: return 'network_denied'
    if any(c and 'effect' in c for c in codes): return 'effect_denied'
    if any(c and ('verif' in c or 'contract' in c) for c in codes): return 'verifier_failed'
    if any(c and ('runtime_unavailable' in c or 'toolchain' in c) for c in codes): return 'runtime_unavailable'
    return 'attempt_failed' if (status.get('search_state') or {}).get('attempts') else 'runtime_unavailable'

class Controller:
    def __init__(self, args):
        self.args = args
        self.plan = read(args.plan)
        require(sha(args.plan) == args.plan_sha256, 'plan drift')
        require(self.plan['maximum_reservation_usd_micros'] <= 4513388, 'budget ceiling')
        require(self.plan['arm']['network'] == 'denied', 'network drift')
        require(self.plan['arm']['max_proposal_rounds'] == self.plan['arm']['max_proposals'] == 1, 'proposal scope')
        self.root = Path(args.run).resolve()
        self.root.mkdir(exist_ok=False)
        (self.root / '.tmp').mkdir()
        self.processes = []
        self.environment = {'PATH': '/opt/ato/toolchains/node/22.14.0/bin:/usr/bin:/bin',
                            'HOME': '/home/ubuntu', 'TMPDIR': str(self.root / '.tmp')}
        self.receiver = self.root / 'receiver'
        self.receiver.mkdir()
        self.token = self.root / 'coordinator-token'
        self.cells = self.root / 'cells'; self.cells.mkdir()
        self.summary = {'schema': 'ato.formation-adaptive-100-raw/1', 'plan_sha256': args.plan_sha256,
                        'preregistration_commit': args.preregistration_commit,
                        'started_at': utc(), 'applications': [], 'pilot_gate': 'pending'}

    def start(self, command, log, cwd=None, requester=False):
        env = self.environment
        if requester:
            require(self.args.credential_socket is not None, 'requester credential channel required')
            command = [sys.executable, self.args.credential_wrapper, self.args.credential_socket, *map(str, command)]
        with Path(log).open('xb') as stream:
            process = subprocess.Popen(list(map(str, command)), cwd=cwd, env=env,
                                       stdin=subprocess.DEVNULL, close_fds=True,
                                       stdout=stream, stderr=subprocess.STDOUT, start_new_session=True)
        self.processes.append(process)
        return process

    def stop(self, process):
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL); process.wait(timeout=10)

    def wait(self, check, why, seconds=60):
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            value = check()
            if value: return value
            time.sleep(.05)
        raise RuntimeError('infrastructure timeout: ' + why)

    def sql(self, query, params):
        number = secrets.token_hex(12)
        folder = self.receiver / 'commands'
        write(folder / (number + '.tmp'), {'sql': query, 'params': params})
        (folder / (number + '.tmp')).rename(folder / (number + '.in.json'))
        out = folder / (number + '.out.json')
        self.wait(out.exists, 'local SQL')
        result = read(out)
        require('error' not in result, 'local SQL failed')
        return result['results']

    def initialize(self):
        for item in self.plan['binaries'].values():
            require(sha(item['path']) == item['sha256'], 'binary drift')
        for app in self.plan['applications']:
            require(sha(app['archive_path']) == app['archive_sha256'], 'source drift')
        require(shutil.disk_usage(self.root).free >= 20 * 1024**3, 'disk gate')
        shutil.copytree(self.plan['receiver_bundle'], self.receiver / 'bundle')
        (self.receiver / 'node_modules').symlink_to(self.plan['api_node_modules'], target_is_directory=True)
        shutil.copyfile(self.plan['coordinator_script'], self.receiver / 'coordinator.mjs')
        self.environment['C2_PORT'] = '19574'
        coordinator = self.start(['node', 'coordinator.mjs'], self.root / 'coordinator.log', self.receiver)
        self.wait(lambda: (self.receiver / 'commands').exists() if coordinator.poll() is None else False,
                  'Coordinator initialization')
        with self.token.open('x') as stream: stream.write('ato_rnr_' + secrets.token_hex(32))
        self.token.chmod(0o600)
        self.sql('INSERT INTO "user"(id,name,email) VALUES(?,?,?)', ['adaptive100', 'adaptive100', 'adaptive100@acceptance.invalid'])
        self.sql('INSERT INTO runner_devices(id,user_id,display_name,token_hash) VALUES(?,?,?,?)',
                 ['local', 'adaptive100', '6b-F local', hashlib.sha256(self.token.read_bytes()).hexdigest()])
        write(self.root / 'start.json', self.summary)

    def app(self, app):
        n = f"{app['index']:03}"
        cell = self.cells / n; cell.mkdir()
        tmp = cell / '.tmp'; tmp.mkdir()
        local_root = cell / 'known'; local_root.mkdir()
        known = cell / 'known.json'
        env = dict(self.environment, TMPDIR=str(tmp), ATO_HOME=str(cell / 'ato-home'))
        command = [self.plan['binaries']['coverage_baseline']['path'], app['archive_path'],
                   'sha256:' + app['archive_sha256'], str(local_root), self.plan['binaries']['worker']['path'], str(known)]
        started = time.monotonic()
        with (cell / 'known.stdout.log').open('xb') as stdout, (cell / 'known.stderr.log').open('xb') as stderr:
            process = subprocess.Popen(command, env=env, stdin=subprocess.DEVNULL,
                                       stdout=stdout, stderr=stderr, start_new_session=True)
            self.processes.append(process)
            try: rc = process.wait(timeout=900)
            except subprocess.TimeoutExpired:
                self.stop(process); rc = process.returncode
        require(rc == 0 and known.exists(), 'known-phase infrastructure error')
        result = read(known)['result']
        value = result.get('result', {})
        attempts = value.get('attempts', [])
        for attempt in attempts:
            if attempt.get('contract_ref'):
                require(attempt['contract_ref'] == app['contract_ref'], 'known-phase K drift')
        receipts = [attempt['receipt'] for attempt in attempts if attempt.get('receipt')]
        passed = any(receipt.get('fully_satisfied') and receipt['contract_ref'] == app['contract_ref']
                     for receipt in receipts)
        row = {'index': app['index'], 'name': app['name'], 'baseline_terminal': app['baseline_terminal'],
               'baseline_typed_K_pass': app['baseline_typed_K_pass'], 'contract_ref': app['contract_ref'],
               'known_result': str(known.relative_to(self.root)), 'known_result_sha256': sha(known),
               'proposal_eligible': app['proposal_eligible'], 'producer_calls': 0, 'decision_calls': 0,
               'adaptive_typed_K_pass': passed, 'generated_D_provenance': [],
               'functional_acceptance': 'not_measured', 'accounting_closed': True,
               'charged_reservation_usd_micros': 0}
        if passed:
            row.update(adaptive_terminal='known_d_pass', no_call_reason='fresh_known_D_PASS')
        elif not app['proposal_eligible']:
            row.update(adaptive_terminal='operation_catalog_gap', no_call_reason='empty_authorized_catalog',
                       gap_detail='preregistered_source_domain_or_operation_vocabulary_gap')
        elif not app['wire_ready']:
            row.update(adaptive_terminal='source_transport_limit',
                       no_call_reason='requester_snapshot_exceeds_existing_source_object_cap',
                       preflight_evidence=app['requester_preflight_failure'])
        else:
            require(all(a['status'] == 'filtered' or not a.get('derivation_ref') for a in attempts),
                    'prior execution history requires same-search integration; stop before provider')
            require(app['wire_ready'], 'offline requester gate failed')
            config = read(app['offline_config'])
            config.update(preregister_only=False, token_file=str(self.token), work=str(cell / 'requester-work'),
                          expected_preregistration=read(app['requester_preregistration']),
                          created=str(cell / 'created.json'), result=str(cell / 'adaptive.json'))
            budget = dict(self.plan['producer_budget'], max_calls=1)
            budget_path = cell / 'producer-budget.json'; write(budget_path, budget)
            journal = cell / 'producer-journal.jsonl'
            created_budget = subprocess.run([self.plan['binaries']['budget']['path'], 'create', str(budget_path), str(journal)],
                                            env=self.environment, capture_output=True, text=True, timeout=30)
            require(created_budget.returncode == 0, 'reservation initialization')
            config['live_llm'] = {'config': self.plan['producer_config'], 'budget': budget, 'journal': str(journal)}
            config['decision'] = dict(self.plan['decision_config'],
                                      request_record=str(cell / 'decision-request.json'), response_record=str(cell / 'decision-response.json'))
            config_path = cell / 'live-config.json'; write(config_path, config)
            rt_root = cell / 'runtime'; rt_root.mkdir()
            rt = self.start([self.plan['binaries']['runtime']['path'], config['api'], self.token,
                             rt_root / 'work', rt_root / 'out', self.plan['binaries']['worker']['path']], rt_root / 'runtime.log')
            try:
                self.wait(lambda: self.sql('SELECT environment_id FROM runtime_environments WHERE runtime_id=?', ['local']), 'Runtime advertisement')
                req = self.start([self.plan['binaries']['requester']['path'], config_path], cell / 'requester.log', requester=True)
                try: code = req.wait(timeout=900)
                except subprocess.TimeoutExpired:
                    self.stop(req); code = req.returncode
                require(code == 0, 'requester infrastructure/design gate failed')
                observed = read(cell / 'adaptive.json')
                require(observed['contract_ref'] == app['contract_ref'], 'adaptive K drift')
                status = observed['status']
                row['search_state'] = status.get('search_state')
                row['adaptive_status'] = status
                row['adaptive_result'] = str((cell / 'adaptive.json').relative_to(self.root))
                row['adaptive_result_sha256'] = sha(cell / 'adaptive.json')
                calls = observed['producer_budget_snapshot']['cells']
                row['producer_calls'] = len(calls)
                require(len(calls) <= 1 and observed['producer_budget_snapshot']['stopped'], 'producer accounting gate')
                row['producer_usage_accounting'] = observed['producer_budget_snapshot']
                row['decision_calls'] = int((cell / 'decision-request.json').exists())
                row['decisions'] = observed['decisions']
                if row['decision_calls']:
                    require((cell / 'decision-response.json').exists(), 'decision accounting unresolved')
                    row['decision_usage_accounting'] = read(cell / 'decision-response.json')
                    require(row['decision_usage_accounting']['accounting_closed'], 'decision settlement gate')
                row['producer_response_unknown'] = sum(x['response'] is None for x in calls.values())
                row['charged_reservation_usd_micros'] = row['producer_calls'] * 81102 + row['decision_calls'] * 2753
                row['reservation_settlement'] = 'full_ceiling_charged_no_refund; halted journal; returned blocking transport'
                row['generated_D_provenance'] = (status.get('proposal_round') or {}).get('outcomes', [])
                row['adaptive_typed_K_pass'] = bool(observed['accepted'])
                row['adaptive_terminal'] = adaptive_terminal(observed, row['producer_calls'])
                row['call_reason'] = 'nonempty_catalog_after_actual_known_D_exhaustion'
                row['no_call_reason'] = None if row['producer_calls'] else 'decision_or_receiver_terminal_before_producer'
            finally:
                self.stop(rt)
        row['elapsed_seconds'] = round(time.monotonic() - started, 3)
        write(cell / 'summary.json', row)
        self.summary['applications'].append(row)
        (self.root / 'results.json').write_text(json.dumps(self.summary, indent=2) + '\n')
        print(n, app['name'], row['adaptive_terminal'], flush=True)
        # Disposable expanded scratch only. Keep raw records, receipts and retained output.
        for path in local_root.glob('source-*'):
            if path.is_dir(): shutil.rmtree(path)
        return row

    def run(self):
        self.initialize()
        pilot = self.plan['pilot_indices']
        by_index = {app['index']: app for app in self.plan['applications']}
        for index in pilot: self.app(by_index[index])
        require(all(x['accounting_closed'] for x in self.summary['applications']), 'pilot accounting gate')
        require(sum(x['producer_calls'] for x in self.summary['applications']) >= 1, 'pilot did not exercise producer')
        require(sum(x['decision_calls'] for x in self.summary['applications']) >= 1, 'pilot did not exercise decision provider')
        self.summary['pilot_gate'] = 'pass'
        write(self.root / 'pilot-gate.json', {'status': 'pass', 'indices': pilot,
                                             'applications': self.summary['applications']})
        for index in range(1, 101):
            if index not in pilot: self.app(by_index[index])
        self.summary['completed_at'] = utc()
        self.summary['completion'] = {'actual_terminals': len(self.summary['applications'])}
        (self.root / 'results.json').write_text(json.dumps(self.summary, indent=2) + '\n')

    def close(self):
        for process in reversed(self.processes): self.stop(process)
        self.token.unlink(missing_ok=True)

def main():
    p = argparse.ArgumentParser()
    for name in ['plan', 'plan_sha256', 'run', 'preregistration_commit', 'credential_wrapper']:
        p.add_argument('--' + name.replace('_', '-'), required=True)
    p.add_argument('--credential-socket')
    p.add_argument('--check-only', action='store_true')
    args = p.parse_args()
    if args.check_only:
        plan = read(args.plan)
        require(sha(args.plan) == args.plan_sha256, 'plan drift')
        require(plan['maximum_reservation_usd_micros'] <= 4513388, 'budget ceiling')
        require(plan['maximum_reservation_usd_micros'] ==
                plan['model_budget']['max_requester_invocations'] * (81102 + 2753), 'reservation arithmetic')
        commit = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=plan['preregistration_worktree'], text=True).strip()
        require(commit == args.preregistration_commit, 'preregistration commit drift')
        frozen = subprocess.check_output(['git', 'show', commit + ':docs/ops/formation-adaptive-100-plan.json'],
                                         cwd=plan['preregistration_worktree'])
        require(hashlib.sha256(frozen).hexdigest() == args.plan_sha256, 'committed plan drift')
        for path, expected in plan['harness_hashes'].items():
            require(sha(Path(plan['preregistration_worktree']) / path) == expected, 'harness drift')
        for name, expected in plan['environment']['receiver_files'].items():
            require(sha(Path(plan['receiver_bundle']) / name) == expected, 'receiver drift')
        for item in plan['binaries'].values(): require(sha(item['path']) == item['sha256'], 'binary drift')
        for app in plan['applications']:
            require(sha(app['archive_path']) == app['archive_sha256'], 'source drift')
            if app['wire_ready']:
                require(sha(app['requester_preregistration']) == app['requester_preregistration_sha256'], 'preregistration drift')
                config = read(app['offline_config'])
                config['live_llm'] = {'config':plan['producer_config'], 'budget':plan['producer_budget'], 'journal':'unused'}
                config['decision'] = dict(plan['decision_config'], request_record='unused', response_record='unused')
                scratch = Path(plan['preregistration_worktree']) / '.tmp' / ('config-gate-' + secrets.token_hex(12) + '.json')
                scratch.parent.mkdir(exist_ok=True)
                write(scratch, config)
                try:
                    result = subprocess.run([plan['binaries']['requester']['path'], scratch, '--validate-config'],
                                            env={'PATH':'/usr/bin:/bin','HOME':'/home/ubuntu'}, capture_output=True, timeout=30)
                    require(result.returncode == 0, 'Rust wire/config validation failed')
                finally: scratch.unlink()
        print(json.dumps({'execution_gate':'pass','model_calls':0,'credential_read':False}))
        return
    controller = None
    try:
        controller = Controller(args); controller.run()
    except Exception as error:
        if controller:
            write(controller.root / 'STOP.json', {'kind': 'infrastructure_or_design_gate', 'message': str(error),
                                                  'completed_apps': len(controller.summary['applications']), 'at': utc()})
        raise
    finally:
        if controller: controller.close()

if __name__ == '__main__': main()
