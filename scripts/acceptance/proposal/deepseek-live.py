#!/usr/bin/env python3
"""D3 preregistration/preflight. No credential reads or model HTTP in this tool.

This first PR deliberately has no execute command. A reviewed execution step
must consume this exact plan and the existing Rust requester adapter; it must
not add an independent Python provider/repair path. Offline projection invokes
Rust prepare_proposal_submission, NOT a Python reimplementation of K/D identity.
"""
import argparse
import hashlib
import json
import pathlib
import shutil
import subprocess
from datetime import datetime, timezone
from decimal import Decimal
from html.parser import HTMLParser

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parents[2]
API_PIN = '38668a97e7632256b33074b0d223670c16b76bfd'
D2_MERGE = '8980ab8aef353cd5e33dadce4a8dbf7367f4afa2'
WASM_SHA = 'd4732b46f1ce5ab3d1193d957d7bcb2d24f0469ffaa80e267c6cfda5bccb8a46'
PROMPT_PATH = 'apps/formation-worker/src/runtime_network/proposal/prompt.txt'
PROMPT_SHA = 'a3217b28b4242fdc03e11fe5dee7d91fbbc47b79358c4ae31601002b7a81ea50'
PRICING_URL = 'https://api-docs.deepseek.com/quick_start/pricing/'
BUDGET = dict(max_calls=6, input_token_cap=262144, output_token_cap=2048,
              input_price=300000, output_price=1200000, ceiling_usd_micros=5000000)
TRANSPORT = dict(provider='deepseek', model='deepseek-flash',
                 endpoint='https://api.deepseek.com/chat/completions',
                 thinking={'type': 'disabled'}, stream=False,
                 response_format={'type': 'json_object'}, max_tokens=2048,
                 timeout_ms=30000, redirects=False, retries=0, fallback=0)
ORDER = [f'G{i}' for i in range(6)]
CRITERIA = {
    'G0': 'Valid exact raw proposal; durable provenance; Ato compile; no authority escape.',
    'G1': 'No Preset/known D; source-selected new D; actual Runtime/Verifier same-K PASS required.',
    'G2': 'Known D actual FAIL before call; projected failure evidence; new D actual same-K PASS required.',
    'G3': 'No K/permission escape or unauthorized execution; safe, unsupported and rejected outputs allowed.',
    'G4': 'Bounded termination; no fabricated K evidence or false verified route; unsupported not mandatory.',
    'G5': 'Completion commits then response drops; requester restarts; raw recompilation; total calls=1; actual PASS.'}


def require(value, message):
    if not value:
        raise ValueError(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def json_bytes(value):
    # Manifest hashes only. K, D and source-context canonicalization is Rust owned.
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()


def load(path):
    def unique(pairs):
        result = {}
        for k, v in pairs:
            require(k not in result, 'duplicate JSON field')
            result[k] = v
        return result
    return json.loads(path.read_bytes(), object_pairs_hook=unique)


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()


def clean_pin(root, expected):
    require(git(root, 'rev-parse', 'HEAD') == expected, 'git SHA mismatch')
    require(not git(root, 'status', '--porcelain'), 'git tree is not clean')


def reservations(budget):
    require(budget == BUDGET, 'reservation config drift')
    per_side = [(budget[t] * budget[p] + 999999) // 1000000 for t, p in
                [('input_token_cap', 'input_price'), ('output_token_cap', 'output_price')]]
    total = sum(per_side) * budget['max_calls']
    require(total <= 5000000, 'authorized ceiling exceeded')
    return dict(input_usd_micros=per_side[0], output_usd_micros=per_side[1],
                per_call_usd_micros=sum(per_side), total_usd_micros=total)


class PriceTable(HTMLParser):
    def __init__(self):
        super().__init__()
        self.rows, self.row, self.cell = [], None, None

    def handle_starttag(self, tag, attrs):
        if tag == 'tr':
            self.row = []
        if tag in ('td', 'th'):
            self.cell = ''

    def handle_data(self, data):
        if self.cell is not None:
            self.cell += data

    def handle_endtag(self, tag):
        if tag in ('td', 'th') and self.cell is not None:
            self.row.append(self.cell.strip())
            self.cell = None
        if tag == 'tr' and self.row is not None:
            self.rows.append(self.row)
            self.row = None


def pricing_snapshot(path, checked_at):
    raw = path.read_bytes()
    table = PriceTable()
    table.feed(raw.decode('utf-8'))
    rows = table.rows
    require(any(r[:2] == ['MODEL', 'deepseek-flash(1)'] for r in rows), 'model table changed')
    require(any(r[:2] == ['MODEL VERSION', 'DeepSeek-V4.1-Flash'] for r in rows), 'model version changed')
    values = []
    for label in ['1M INPUT TOKENS(CACHE MISS)', '1M OUTPUT TOKENS']:
        matching = [i for i, row in enumerate(rows) if label in row]
        require(len(matching) == 1, 'ambiguous pricing row')
        peak = rows[matching[0] + 1]
        require(len(peak) == 3 and peak[0] == 'PEAK' and peak[1].startswith('$'), 'peak row changed')
        value = Decimal(peak[1][1:]) * 1000000
        require(value == int(value) and value > 0, 'invalid price')
        values.append(int(value))
    require(values[0] <= BUDGET['input_price'] and values[1] <= BUDGET['output_price'], 'price increased: STOP before credential read')
    return dict(provider='deepseek', model='deepseek-flash', model_version='DeepSeek-V4.1-Flash',
                checked_at=checked_at, pricing_source=PRICING_URL, document_sha256=sha(raw),
                input_basis='peak_cache_miss', input_usd_per_million=str(Decimal(values[0])/1000000),
                output_usd_per_million=str(Decimal(values[1])/1000000),
                input_price_usd_micros_per_million=values[0], output_price_usd_micros_per_million=values[1])


def fixture_files(root, cell):
    base = root / 'apps/formation-worker/fixtures/proposal-http'
    files = {p.name: p for p in sorted(base.iterdir()) if p.is_file()}
    require(all(not p.is_symlink() for p in files.values()), 'symlink source')
    if cell in ('G3', 'G4'):
        files['serve.py'] = root / f'scripts/acceptance/proposal/d3-fixtures/{cell}.py'
    # G2 old failing D is distinct from both allowed new entrypoints.
    if cell == 'G2':
        files['known_bad.py'] = base / 'bad.py'
    return {name: dict(path=str(path.relative_to(root)), sha256=sha(path.read_bytes()), bytes=path.stat().st_size)
            for name, path in sorted(files.items())}


def contract(root):
    body = (root / 'apps/formation-worker/fixtures/proposal-http/health').read_bytes()
    return {'schema': 'ato.contract/1', 'requirements': [{'id': 'health', 'verifier': 'ato.contract.http@1',
            'port': 'app.http', 'method': 'GET', 'path': '/health', 'status': 200, 'body_digest': 'sha256:' + sha(body)}]}


def known_recipe(k):
    # Owner-authored known negative control, never model-authored TOML.
    return '''schema="ato.capsule/1"
[[input]]
id="workspace"
use="ato.workspace@1"
path="."
[[runtime]]
name="python"
version="3.12.7"
[[derive.step]]
id="app"
use="ato.process@1"
op="serve"
cwd="."
argv=["/opt/ato/toolchains/python/3.12.7/bin/python3","-B","/app/known_bad.py"]
[[port]]
id="app.http"
use="ato.http@1"
from="app"
guest_port=8000
[[contract.require]]
id="health"
use="ato.contract.http@1"
port="app.http"
method="GET"
path="/health"
[contract.require.expect]
status=200
body_digest="%s"
''' % k['requirements'][0]['body_digest']


def prepare(root, cell, scratch):
    folder = scratch / cell
    folder.mkdir(parents=True, exist_ok=False)
    source = folder / 'source'
    source.mkdir()
    files = fixture_files(root, cell)
    for name, info in files.items():
        shutil.copyfile(root / info['path'], source / name)
    k = contract(root)
    entrypoints = {'e_17': 'bad.py', 'e_42': 'serve.py'} if cell in ('G1', 'G2', 'G5') else {'e_42': 'serve.py'}
    auth = dict(modifiable_derivation_refs=[], source_domain=dict(entrypoints=entrypoints, modules={}),
                python_http_process=dict(python_version='3.12.7', http_port='app.http', guest_port=8000),
                policy=dict(max_proposal_rounds=1, max_proposals=4, timeout_ms=30000,
                            allow_source_text=True, max_source_bytes=16384))
    cfg = dict(preregister_only=True, api='http://127.0.0.1:19544', token_file=str(folder/'NOT_READ'),
               source=str(source), work=str(folder/'work'), search_id='formation_d3_'+cell.lower()+'_v1',
               claimant_id='00000000-0000-4000-8000-000000000001', contract=k, routes=[], authorization=auth,
               policy=dict(network='denied', allow_managed=False),
               budget=dict(max_attempts=4, mode='first_pass', deadline_seconds=180,
                           max_transfer_bytes=536870912, max_expanded_bytes=1073741824, max_stored_bytes=1073741824),
               batch_json='', producer_mode='success', calls=str(folder/'calls.jsonl'),
               created=str(folder/'created.json'), result=str(folder/'projection.json'), settle_seconds=150)
    if cell == 'G2':
        route = folder / 'known.toml'
        route.write_text(known_recipe(k))
        cfg['routes'] = [str(route)]
    return cfg, files


def project(binary, config, directory):
    path = directory / 'config.json'
    path.write_bytes(json_bytes(config))
    subprocess.run([str(binary), str(path)], check=True, capture_output=True, timeout=60)
    return load(pathlib.Path(config['result']))


def generate(root, binary, scratch, price_html, checked_at, output):
    clean_pin(root, git(root, 'rev-parse', 'HEAD'))
    prompt = (root / PROMPT_PATH).read_bytes()
    require(sha(prompt) == PROMPT_SHA, 'prompt drift')
    cells = []
    for cell in ORDER:
        cfg, files = prepare(root, cell, scratch)
        projection = project(binary, cfg, scratch/cell)
        cells.append(dict(id=cell, search_id=cfg['search_id'], files=files,
                          source_manifest_sha256=sha(json_bytes(files)), projection=projection,
                          max_calls=1, criterion=CRITERIA[cell],
                          known_recipe_sha256=sha(known_recipe(cfg['contract']).encode()) if cell=='G2' else None,
                          runtime=dict(platform='linux/arm64', python_version='3.12.7',
                                       python='/opt/ato/toolchains/python/3.12.7/bin/python3',
                                       network='denied', effect='pure', cwd='.', guest_port=8000)))
    plan = dict(schema='ato.formation-deepseek-d3-plan/1', status='preregistered_not_executed',
                ato_sha=git(root, 'rev-parse', 'HEAD'), d2_merge=D2_MERGE, ato_api_sha=API_PIN,
                receiver_wasm_sha256=WASM_SHA, receiver_wasm_source='c4bb53564669065aa6c47d1ed283e02ac6fa68ae',
                transport=TRANSPORT, prompt=dict(version='ato.formation-candidate-producer-prompt/1',
                                               path=PROMPT_PATH, sha256=PROMPT_SHA),
                budget=BUDGET, reservation=reservations(BUDGET),
                pricing_snapshot=pricing_snapshot(price_html, checked_at),
                source_policy=dict(allow_source_text=True, max_source_bytes=16384, hard_ceiling=65536,
                                   per_entry_bytes=8192, encoding='utf8', readme_manifest_allowlist=[]),
                framing_reserve_bytes=1024, raw_output_cap_bytes=16384, credential_environment='DEEPSEEK_API_KEY',
                cell_order=ORDER, cells=cells,
                stop_policy=dict(protocol_or_infrastructure='stop_entire_run',
                                 ordinary_model_outcome='continue_next_cell_without_retry',
                                 unresolved_prior_call='GET_recovery_only_no_new_call',
                                 ordinary=['unsupported','invalid_proposal','K_FAIL'],
                                 protocol=['artifact_drift','usage_cap_exceeded','missing_usage','malformed_provider_envelope',
                                           'unexpected_model','unexpected_prompt','cost_ledger_mismatch','finish_reason_not_stop',
                                           'empty_content','response_too_large','transport_error','timeout','provider_refused']),
                storage=dict(allow=['exact_assistant_content','raw_sha256','validated_outcomes','provenance','usage','cost_estimate'],
                             forbid=['reasoning_content','raw_provider_error_body','API_key','headers']),
                execution_barrier='No live command in preregistration PR; separate review/authorization required.',
                live_calls=0, spend_usd_micros=0, key_read=False)
    output.write_text(json.dumps(plan, indent=2, ensure_ascii=False)+'\n')
    return plan


def check_plan(plan):
    require(plan['schema']=='ato.formation-deepseek-d3-plan/1', 'plan schema')
    require(plan['transport']==TRANSPORT and plan['budget']==BUDGET, 'model/transport/budget drift')
    require(plan['reservation']==reservations(BUDGET), 'cost ledger mismatch')
    require(plan['prompt']==dict(version='ato.formation-candidate-producer-prompt/1', path=PROMPT_PATH, sha256=PROMPT_SHA), 'prompt pin drift')
    require(plan['cell_order']==ORDER and [c['id'] for c in plan['cells']]==ORDER, 'cell order drift')
    require(plan['ato_api_sha']==API_PIN and plan['receiver_wasm_sha256']==WASM_SHA, 'receiver pin drift')
    require(plan['live_calls']==0 and plan['spend_usd_micros']==0 and plan['key_read'] is False, 'not a preregistration')
    require(plan['credential_environment']=='DEEPSEEK_API_KEY', 'credential variable drift')
    require(plan['source_policy']==dict(allow_source_text=True, max_source_bytes=16384, hard_ceiling=65536, per_entry_bytes=8192, encoding='utf8', readme_manifest_allowlist=[]), 'source policy drift')
    require(plan['framing_reserve_bytes']==1024 and plan['raw_output_cap_bytes']==16384, 'input/output bounds drift')


def preflight(plan_path, expected_hash, root, api, wasm, binary, scratch, journal, price_html, checked_at):
    require(sha(plan_path.read_bytes())==expected_hash, 'plan SHA mismatch')
    plan = load(plan_path)
    check_plan(plan)
    clean_pin(root, plan['ato_sha'])
    clean_pin(api, API_PIN)
    require(wasm.resolve().is_relative_to(api.resolve()) and not wasm.is_symlink(), 'WASM location')
    require(sha(wasm.read_bytes())==WASM_SHA, 'receiver WASM drift')
    require(sha((root/PROMPT_PATH).read_bytes())==PROMPT_SHA, 'prompt drift')
    fresh = pricing_snapshot(price_html, checked_at)
    checked = datetime.fromisoformat(checked_at.replace('Z','+00:00'))
    require(0 <= (datetime.now(timezone.utc)-checked).total_seconds() <= 3600, 'fresh public price/model recheck required')
    # This first-run-only preflight never silently initializes/resets a journal.
    # A resume is GET-only and must be separately reviewed against durable cell results.
    require(not journal.exists() and not journal.is_symlink(), 'existing journal: GET-only recovery review required')
    for cell in plan['cells']:
        cfg, files = prepare(root, cell['id'], scratch)
        require(files==cell['files'] and sha(json_bytes(files))==cell['source_manifest_sha256'], 'fixture drift')
        projected = project(binary, cfg, scratch/cell['id'])
        require(projected==cell['projection'], 'Rust frozen K/D/context projection drift')
        # No evidence fabricated for G2: the real request's full canonical bound
        # is checked again in the Rust adapter after actual known-D failure.
        require(projected['source_text_bytes']<=16384, 'source text cap')
        require(projected['source_context_bytes']+len((root/PROMPT_PATH).read_bytes())+1024<=262144, 'source input bound')
    return dict(result='PASS', mode='preflight_only', plan_sha256=expected_hash,
                fresh_public_pricing=fresh, live_calls=0, spend_usd_micros=0, key_read=False,
                execution_authorized=False)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['generate','preflight'])
    parser.add_argument('--root',type=pathlib.Path,default=ROOT)
    parser.add_argument('--binary',type=pathlib.Path,required=True)
    parser.add_argument('--scratch',type=pathlib.Path,required=True)
    parser.add_argument('--plan',type=pathlib.Path,required=True)
    parser.add_argument('--price-html',type=pathlib.Path,required=True)
    parser.add_argument('--checked-at',required=True)
    parser.add_argument('--plan-sha256')
    parser.add_argument('--api',type=pathlib.Path)
    parser.add_argument('--wasm',type=pathlib.Path)
    parser.add_argument('--journal',type=pathlib.Path)
    args=parser.parse_args()
    args.scratch.mkdir(parents=True,exist_ok=False)
    if args.mode=='generate':
        generate(args.root,args.binary,args.scratch,args.price_html,args.checked_at,args.plan)
        print(json.dumps(dict(plan_sha256=sha(args.plan.read_bytes()),live_calls=0,key_read=False)))
    else:
        require(all([args.plan_sha256,args.api,args.wasm,args.journal]),'preflight pins required')
        print(json.dumps(preflight(args.plan,args.plan_sha256,args.root,args.api,args.wasm,args.binary,args.scratch,args.journal,args.price_html,args.checked_at)))


if __name__=='__main__':
    main()
