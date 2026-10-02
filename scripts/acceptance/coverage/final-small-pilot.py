#!/usr/bin/env python3
"""Actual isolated small gate; aggregate existing product call ledgers."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import time
from urllib.request import Request, urlopen

spec = importlib.util.spec_from_file_location('pilot', Path(__file__).with_name('reasoning-pilot.py'))
pilot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pilot)

class FinalPilot(pilot.Pilot):
    def __init__(self, args):
        self.root_processes = set()
        super().__init__(args)

    def start(self, cmd, cwd, stdout, stderr=None):
        root = (self.plan.get('runtime_owner_root') is True and len(cmd) > 2
                and list(cmd[1:3]) == ['runtime-network', 'serve'])
        if root:
            cmd = ['sudo', '-n', '--', '/usr/bin/env', '-i',
                   *[name+'='+self.env[name] for name in ('PATH','HOME','TMPDIR')], *cmd]
        process = super().start(cmd, cwd, stdout, stderr)
        if root:
            self.root_processes.add(process.pid)
        return process

    def stop(self, process):
        if process.pid in self.root_processes and process.poll() is None:
            subprocess.run(['sudo','-n','kill','-TERM','--','-'+str(process.pid)], check=True)
            try:
                process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                subprocess.run(['sudo','-n','kill','-KILL','--','-'+str(process.pid)], check=True)
                process.wait()
        else:
            super().stop(process)

    def app(self, app):
        super().app(app)
        cell = self.root/'cells'/f"{app['index']:03}"
        suffix = '.'+self.suffix if self.a.resume else ''
        status = pilot.read(cell/('status'+suffix+'.json'))
        def acknowledged(view):
            expected = {a['attempt_id'] for a in view['attempts']
                        if a['status'] in ('pass', 'fail', 'inconclusive')}
            acknowledged_ids = set()
            for delivery in (Path(self.a.runtime_root)/'out/delivery').iterdir():
                if not delivery.is_dir() or not (delivery/'closed.json').exists():
                    continue
                if (delivery/'result.response.json').exists() and (delivery/'claim.response.json').exists():
                    ticket = pilot.read(delivery/'claim.response.json')
                    if ticket:
                        acknowledged_ids.add(ticket['attempt_id'])
            return expected <= acknowledged_ids
        if status['status'] != 'running' and acknowledged(status):
            return
        # Requester timeout does not finish a claimed Runtime operation. Keep
        # the receiver alive for bounded reporting/settlement after expiry;
        # never resume the requester or renew its frozen execution allowance.
        headers = {'Authorization': 'Bearer '+self.token.read_text().strip()}
        end = time.monotonic()+60
        while time.monotonic() < end:
            with urlopen(Request(self.plan['api']+'/v1/runtime-network/satisfy/'+status['satisfy_id'],
                                 headers=headers), timeout=5) as response:
                status = json.load(response)
            if status['status'] != 'running' and acknowledged(status):
                pilot.write(cell/('status.reporting-final'+suffix+'.json'), status)
                row = pilot.read(cell/('summary'+suffix+'.json'))
                pilot.write(cell/('summary.reporting-final'+suffix+'.json'), {
                    **row, 'coordinator_status': status['status'],
                    'reporting_after_requester_exit': True,
                    'requester_resumed': False,
                })
                return
            if self.runtime.poll() is not None:
                raise RuntimeError('Runtime stopped before durable settlement; preserve evidence and retry ledger')
            time.sleep(.2)
        raise RuntimeError('bounded post-requester reporting did not settle; preserve evidence and original limits')

    def budget_gate(self):
        if self.plan['producer_config']['provider'] == 'codex_session':
            return super().budget_gate()
        roots = set(map(Path, self.plan['accounting_roots'])) | {self.root}
        count = spent = unsettled = 0
        for root in sorted(roots):
            for journal in sorted(root.glob('cells/*/producer.jsonl')):
                view = json.loads(subprocess.check_output([
                    self.plan['binaries']['preflight']['path'], '--journal-accounting', str(journal)],
                    cwd=self.root, env=self.env))
                count += view['reserved_call_slots']
                spent += view['estimated_cost_usd_micros']
                unsettled += view['unsettled_reservation_usd_micros']
        forecast = self.plan['producer_budget']
        pilot.require(count + forecast['max_calls'] <= self.plan['model_budget']['max_candidate_producer_calls'], 'aggregate call cap')
        pilot.require(unsettled == 0, 'existing provider reservation unresolved')
        pilot.require(spent + forecast['ceiling_usd_micros'] <= self.plan['maximum_reservation_usd_micros'], 'aggregate actual charge plus reservation cap')
        print(json.dumps({'historical_and_current_call_slots':count,'estimated_cost_usd_micros':spent,
                          'unsettled_reservation_usd_micros':unsettled,'next_search_max_calls':forecast['max_calls']}), flush=True)

if __name__ == '__main__':
    p = argparse.ArgumentParser()
    for name in ('plan','plan-sha256','run','runtime-root','indices'):
        p.add_argument('--'+name, required=True)
    p.add_argument('--check-only', action='store_true')
    p.add_argument('--credential-socket')
    p.add_argument('--resume', action='store_true')
    FinalPilot(p.parse_args()).run()
