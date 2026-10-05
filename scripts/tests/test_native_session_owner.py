import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import time
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('native_owner', ROOT / 'scripts/acceptance/coverage/native-session-owner.py')
OWNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(OWNER)

def view(rounds=1, reserved=0, state='running'):
    return {'search_id': 'search', 'configuration_ref': 'sha256:fixture', 'deadline_ms': 123,
            'exchanges_used': rounds, 'inspection_exchanges_completed': 0, 'search_elapsed_ms': 1000,
            'progress': {'status': state, 'rounds_consumed': rounds,
                         'search_budget': {'attempts': {'used': 0, 'reserved': reserved}}}}

class OwnerAdmissionTests(unittest.TestCase):
    def setUp(self):
        (ROOT / '.tmp').mkdir(exist_ok=True)
        self.tmp = tempfile.TemporaryDirectory(dir=ROOT / '.tmp')
        self.root = Path(self.tmp.name)
        self.ledger = self.root / 'ledger.json'
        self.plan = {'schema': 'ato.native-acceptance-campaign/1', 'measurement_id': 'fixture',
                     'initial_campaign': True, 'aggregate_ceiling': {k: 10 for k in OWNER.KEYS}}
        OWNER.initialize(self.plan, self.ledger)
        self.config = {'schema': 'ato.formation-exploration-config/2', 'provider': {'provider': 'agent_session'},
                       'provider_budget': {'max_calls': 6}, 'exploration': {
                           'formation': {'max_rounds': 3}, 'max_provider_calls': 6, 'max_inspections': 4}}
        self.config_path = self.root / 'config.json'
        self.config_path.write_text(json.dumps(self.config))
        self.args = SimpleNamespace(ato=Path('ato'), exploration_config=self.config_path,
            max_attempts=4, deadline_seconds=10, source=self.root / 'source', api='http://127.0.0.1:1',
            token_file=self.root / 'token', exact_runtime='local', connection=self.root / 'connection.json',
            work_root=self.root / 'requester', max_transfer_bytes=100, max_expanded_bytes=100, max_stored_bytes=100)
    def tearDown(self):
        self.tmp.cleanup()
    def entry(self, snapshots=None):
        return {'start_state': 'launched', 'connection': str(self.args.connection), 'snapshots': snapshots or [],
                'reservation': {'searches': 1, 'D_rounds': 3, 'exchanges': 6,
                                'Runtime_attempts': 4, 'inspections': 4, 'sequential_elapsed_seconds': 10}}
    def test_full_scope_is_durable_before_Search_process_creation(self):
        def spawn(command, **kwargs):
            ledger = OWNER.read_ledger(self.plan, self.ledger)
            self.assertEqual(ledger['entries'][0]['start_state'], 'prepared')
            self.assertEqual(ledger['entries'][0]['reservation']['D_rounds'], 3)
            self.assertIn('--session-bridge', command)
            return SimpleNamespace(pid=123)
        report = OWNER.guarded_start(self.plan, self.ledger, self.args, spawn)
        self.assertTrue(report['Search_process_started'])
    def test_campaign_wall_clock_does_not_extend_Search_deadline(self):
        self.args.deadline_seconds = 5
        self.args.wall_clock_seconds = 10
        def spawn(command, **kwargs):
            ledger = OWNER.read_ledger(self.plan, self.ledger)
            entry = ledger['entries'][0]
            self.assertEqual(entry['reservation']['sequential_elapsed_seconds'], 10)
            self.assertEqual(entry['Search_deadline_seconds'], 5)
            self.assertEqual(command[command.index('--deadline-seconds') + 1], '5')
            return SimpleNamespace(pid=124)
        self.assertTrue(OWNER.guarded_start(self.plan, self.ledger, self.args, spawn)['admitted'])
    def test_expired_unknown_observation_is_covered_only_by_campaign_time(self):
        self.plan['aggregate_ceiling']['sequential_elapsed_seconds'] = 2400
        ledger = OWNER.read_ledger({'schema': 'ato.native-acceptance-campaign/1', 'measurement_id': 'fixture',
            'initial_campaign': True, 'aggregate_ceiling': {k: 10 for k in OWNER.KEYS}}, self.ledger)
        entry = self.entry();entry['reservation']['sequential_elapsed_seconds'] = 2400
        ledger['entries'] = [entry]
        saved = view();saved['search_elapsed_ms'] = 1850000
        report = OWNER.refresh(self.plan, ledger, Path('ato'), reader=lambda _: saved)
        self.assertTrue(report['admitted'])
        self.assertEqual(report['used']['sequential_elapsed_seconds'], 1850)
        self.assertEqual(report['held']['sequential_elapsed_seconds'], 550)
        self.assertEqual(entry['snapshots'][0]['deadline_ms'], 123)
    def test_unsatisfied_Search_is_not_a_reusable_Search_slot(self):
        ledger = OWNER.read_ledger(self.plan, self.ledger)
        ledger['entries'] = [self.entry()]
        report = OWNER.refresh(self.plan, ledger, Path('ato'), reader=lambda _: view(state='unsatisfied'))
        self.assertEqual(report['used']['searches'], 1)
        self.assertEqual(report['held']['Runtime_attempts'], 0)
    def test_reconcile_report_uses_owner_wall_clock_without_changing_Search(self):
        self.plan['aggregate_ceiling']['sequential_elapsed_seconds'] = 2400
        self.ledger = self.root / 'reconcile-ledger.json'
        OWNER.initialize(self.plan, self.ledger)
        ledger = OWNER.read_ledger(self.plan, self.ledger)
        entry = self.entry();entry['Search_deadline_seconds'] = 1800
        entry['reservation']['sequential_elapsed_seconds'] = 2400
        ledger['entries'] = [entry];OWNER.publish(self.ledger, ledger)
        original = int(time.time() * 1000) - 1000
        saved = view(state='unknown');saved['deadline_ms'] = original
        execute = Mock(return_value=SimpleNamespace(returncode=0))
        args = SimpleNamespace(ato=Path('ato'), connection=self.args.connection,
            native_launcher=Path('native-session.py'), native_args=['--', '--reconcile-only'])
        real_refresh = OWNER.refresh
        with patch.object(OWNER, 'refresh', side_effect=lambda p,l,a: real_refresh(p,l,a,reader=lambda _:saved)):
            self.assertTrue(OWNER.guarded_native(self.plan, self.ledger, args, execute)['admitted'])
        command = execute.call_args.args[0]
        self.assertEqual(int(command[command.index('--owner-wall-clock-deadline-ms')+1]), original+600000)
        self.assertEqual(OWNER.read_ledger(self.plan,self.ledger)['entries'][0]['snapshots'][0]['deadline_ms'],original)
    def test_old_overrun_is_not_reset_and_prevents_process_creation(self):
        ledger = OWNER.read_ledger(self.plan, self.ledger)
        ledger['historical_floor']['D_rounds'] = 13
        OWNER.publish(self.ledger, ledger)
        spawn = Mock()
        report = OWNER.guarded_start(self.plan, self.ledger, self.args, spawn)
        self.assertFalse(report['admitted']); spawn.assert_not_called()
        self.assertEqual(OWNER.read_ledger(self.plan, self.ledger)['historical_floor']['D_rounds'], 13)
    def test_incomplete_previous_start_or_status_prevents_new_process(self):
        ledger = OWNER.read_ledger(self.plan, self.ledger)
        ledger['entries'] = [dict(self.entry(), start_state='prepared')]
        OWNER.publish(self.ledger, ledger)
        spawn = Mock()
        with self.assertRaisesRegex(ValueError, 'requires_reconciliation'):
            OWNER.guarded_start(self.plan, self.ledger, self.args, spawn)
        spawn.assert_not_called()
        ledger['entries'][0]['start_state'] = 'launched'
        with self.assertRaises(KeyError):
            OWNER.refresh(self.plan, ledger, Path('ato'), reader=lambda _: {})
    def test_cancelled_unanswered_and_stale_views_never_reduce_history(self):
        ledger = OWNER.read_ledger(self.plan, self.ledger)
        ledger['entries'] = [self.entry([view(3, reserved=4, state='cancelled')])]
        report = OWNER.refresh(self.plan, ledger, Path('ato'), reader=lambda _: view(1, state='cancelled'))
        self.assertEqual(report['used']['D_rounds'], 3)
        self.assertEqual(report['used']['Runtime_attempts'], 4)
        self.assertEqual(report['held']['D_rounds'], 0)
    def test_active_Search_reserves_remaining_scope_not_just_completed_responses(self):
        ledger = OWNER.read_ledger(self.plan, self.ledger)
        ledger['entries'] = [self.entry()]
        report = OWNER.refresh(self.plan, ledger, Path('ato'), reader=lambda _: view(1, reserved=1))
        self.assertEqual(report['used']['D_rounds'] + report['held']['D_rounds'], 3)
        self.assertEqual(report['used']['Runtime_attempts'] + report['held']['Runtime_attempts'], 4)
    def test_old_overrun_prevents_Native_execution(self):
        execute = Mock()
        args = SimpleNamespace(ato=Path('ato'), connection=self.args.connection,
                               native_launcher=Path('native-session.py'), native_args=[])
        ledger = OWNER.read_ledger(self.plan, self.ledger)
        ledger['historical_floor']['D_rounds'] = 13
        OWNER.publish(self.ledger, ledger)
        report = OWNER.guarded_native(self.plan, self.ledger, args, execute)
        self.assertFalse(report['admitted']); execute.assert_not_called()
        del ledger['historical_floor']['D_rounds']
        with self.assertRaisesRegex(ValueError, 'history_incomplete'):
            OWNER.refresh(self.plan, ledger, Path('ato'))
    def test_plan_or_config_changes_do_not_keep_old_admission(self):
        changed = dict(self.plan, aggregate_ceiling={k: 99 for k in OWNER.KEYS})
        with self.assertRaisesRegex(ValueError, 'binding_changed'):
            OWNER.read_ledger(changed, self.ledger)
        self.config['exploration']['formation']['max_rounds'] = 11
        self.config_path.write_text(json.dumps(self.config))
        spawn = Mock()
        report = OWNER.guarded_start(self.plan, self.ledger, self.args, spawn)
        self.assertFalse(report['admitted']); spawn.assert_not_called()

if __name__ == '__main__':
    unittest.main()
