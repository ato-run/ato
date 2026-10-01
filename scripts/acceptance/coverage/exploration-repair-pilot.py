"""Run preregistered zero-known-D subset and separate prior-live-D repair gate."""
import argparse, importlib.util, json
from pathlib import Path
spec=importlib.util.spec_from_file_location('wave',Path(__file__).with_name('exploration-wave.py'))
wave=importlib.util.module_from_spec(spec);spec.loader.exec_module(wave)
p=argparse.ArgumentParser()
for f in ('plan','plan-sha256','replay-plan','replay-plan-sha256','run','runtime-root','replay-run','replay-runtime-root'):p.add_argument('--'+f,required=True)
p.add_argument('--check-only',action='store_true');p.add_argument('--credential-socket');p.add_argument('--resume',action='store_true')
a=p.parse_args()
assert not a.resume,'pilot never resets an existing search'
for replay in (False,True):
 b=argparse.Namespace(plan=a.replay_plan if replay else a.plan,plan_sha256=a.replay_plan_sha256 if replay else a.plan_sha256,
  run=a.replay_run if replay else a.run,runtime_root=a.replay_runtime_root if replay else a.runtime_root,credential_socket=a.credential_socket,
  resume=False,indices='57' if replay else '2,57,69')
 w=wave.Wave(b)
 if a.check_only:continue
 try:
  w.setup()
  for app in w.plan['applications']:w.app(app)
 finally:
  for child in reversed(w.processes):w.stop(child)
print('NON_SECRET_PILOT_GATE_PASS' if a.check_only else 'PILOT_COMPLETE',flush=True)
