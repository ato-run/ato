#!/usr/bin/env python3
"""Small, preregistered shared-session/API arm of the real exploration runner.

Reuse the existing isolated Coordinator/Runtime startup. No manual authoring,
source startup, synthetic receipt, or hidden dependency/build steps.
"""
import argparse
import importlib.util
import json
import subprocess
import time
from pathlib import Path
from urllib.request import Request, urlopen

spec = importlib.util.spec_from_file_location('wave', Path(__file__).with_name('exploration-wave.py'))
wave = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wave)
read, write, sha, require = wave.read, wave.write, wave.sha, wave.require


class Pilot(wave.Wave):
    def budget_gate(self):
        if self.plan['producer_config']['provider'] == 'codex_session':
            require(not self.a.credential_socket, 'session must not receive API credentials')
            return
        roots = [Path(p) for p in self.plan.get('accounting_roots', [])]
        if self.root not in roots:
            roots.append(self.root)
        count = reservation = 0
        for root in roots:
            for journal in sorted(root.glob('cells/*/producer.jsonl')):
                view = json.loads(subprocess.check_output([self.plan['binaries']['preflight']['path'],
                    '--journal-snapshot', str(journal)], env=self.env, cwd=self.root))
                count += len(view['snapshot']['cells'])
                reservation += len(view['snapshot']['cells']) * view['per_call_reservation']
        require(count + self.plan['producer_budget']['max_calls'] <= self.plan['model_budget']['max_candidate_producer_calls'], 'global call cap')
        require(reservation + self.plan['producer_budget']['ceiling_usd_micros'] <= self.plan['maximum_reservation_usd_micros'], 'global cost cap')

    def app(self, app):
        require(not app.get('gate_routes'), 'shared prototype requires zero known D')
        n = f"{app['index']:03}"
        cell = self.root / 'cells' / n
        continuation = self.a.resume and cell.exists()
        cell.mkdir(exist_ok=continuation)
        self.budget_gate()
        config = self.plan['preflight_config']
        if not continuation:
            write(cell/'preflight-config.json', config)
            preflight = self.start([self.plan['binaries']['preflight']['path'], app['archive_path'],
                'sha256:'+app['archive_sha256'], cell/'scratch', 'search_preregister_'+n,
                cell/'preflight-config.json', cell/'preflight.json'], self.root,
                cell/'preflight.stdout.log', cell/'preflight.stderr.log')
            require(preflight.wait(timeout=600) == 0, 'preflight infrastructure failure')
        projected = read(cell/'preflight.json')
        require(projected['contract_ref'] == app['contract_ref'], 'K changed')
        live = {'schema':'ato.formation-exploration-config/2', 'contract':projected['contract'],
            'exploration': config['exploration'], 'toolchains':config['toolchains'],
            'provider':self.plan['producer_config'], 'provider_budget':self.plan['producer_budget'],
            'provider_journal':str(cell/'producer.jsonl')}
        if self.plan['producer_config']['provider'] != 'codex_session':
            live['credential_environment'] = 'DEEPSEEK_API_KEY'
        if not continuation:
            write(cell/'config.json', live)
        else:
            require(read(cell/'config.json') == live, 'config changed across restart')
        cmd = [self.plan['binaries']['ato']['path'], 'form', projected['source'], '--runtime-network',
            '--exploration-config', cell/'config.json', '--api', self.plan['api'], '--token-file', self.token,
            '--exact-runtime', 'local', '--network', 'denied', '--work-root', cell/'requester',
            '--max-attempts', '4', '--deadline-seconds', '900', '--max-transfer-bytes', str(1024**3),
            '--max-expanded-bytes', str(2*1024**3), '--max-stored-bytes', str(1024**3)]
        if continuation:
            cmd += ['--search-id', read(cell/'producer.search.json')['search_id']]
        if self.a.credential_socket:
            cmd = ['python3', self.plan['credential_wrapper'], self.a.credential_socket, *cmd]
        suffix = '.'+self.suffix if continuation else ''
        output = cell/('requester'+suffix+'.stdout.log')
        started = time.monotonic()
        requester = self.start(cmd, self.root, output, cell/('requester'+suffix+'.stderr.log'))
        rc = requester.wait(timeout=950)
        checkpoint = read(cell/'producer.search.json')
        headers = {'Authorization':'Bearer '+self.token.read_text().strip()}
        with urlopen(Request(self.plan['api']+'/v1/runtime-network/exploration/'+checkpoint['search_id']+'/resume', headers=headers)) as response:
            locator = json.load(response)
        with urlopen(Request(self.plan['api']+'/v1/runtime-network/satisfy/'+locator['satisfy_id'], headers=headers)) as response:
            status = json.load(response)
        write(cell/('status'+suffix+'.json'), status)
        try:
            result = read(output)
        except json.JSONDecodeError:
            result = None
        if result is not None:
            require(result['contract_ref'] == app['contract_ref'], 'result K changed')
            require(result['approval'] == 'not_assessed' and result['deployed'] is False, 'submission became execution permission')
        row = {'index': app['index'], 'name':app['name'], 'provider': self.plan['producer_config']['provider'],
            'known_D':False, 'typed_K_pass':result is not None and result['submission'] is not None,
            'rounds_consumed': result.get('rounds_consumed') if result else None,
            'reasoning_accounting':result.get('reasoning_accounting') if result else None,
            'terminal_origin':'requester_verified' if result else 'untyped_requester_failure_and_durable_coordinator',
            'coordinator_status':status['status'], 'automatic_retry':False,
            'result':str(output.relative_to(self.root)), 'result_sha256':sha(output),
            'exit_code':rc, 'elapsed_seconds':round(time.monotonic()-started,3),
            'functional_acceptance':'not_measured', 'deployed':False}
        write(cell/('summary'+suffix+'.json'), row)
        print(json.dumps(row), flush=True)
        # Preserve scratch too for this small prototype, particularly UNKNOWN.


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--plan', required=True)
    parser.add_argument('--plan-sha256', required=True)
    parser.add_argument('--run', required=True)
    parser.add_argument('--runtime-root', required=True)
    parser.add_argument('--indices', required=True)
    parser.add_argument('--check-only', action='store_true')
    parser.add_argument('--credential-socket')
    parser.add_argument('--resume', action='store_true')
    Pilot(parser.parse_args()).run()
