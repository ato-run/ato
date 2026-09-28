"""Read-only A–M barriers; public documentation GET is not model transport."""
import hashlib
import importlib.util
import json
import pathlib
import subprocess
import urllib.request
from datetime import datetime, timezone

PLAN_SHA = 'b67607c6cff1de5339f79822bd3e5d6610d1663dcce6e3f0d6b418ef8a2a1585'
ATO_SHA = '10ad1cd8338e8261cc5bd5e944c38867f22bfe8b'
API_SHA = '38668a97e7632256b33074b0d223670c16b76bfd'
WASM_SHA = 'd4732b46f1ce5ab3d1193d957d7bcb2d24f0469ffaa80e267c6cfda5bccb8a46'
PRICE_URL = 'https://api-docs.deepseek.com/quick_start/pricing/'
CAPTURE_BLOCKER = ('execution pin has no post-claim provider-request capture hook; '
                   'preclaim reconstruction is not an exact provider request SHA')


class Stop(RuntimeError):
    pass


def require(ok, message):
    if not ok:
        raise Stop(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def load(path):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, 'duplicate JSON field')
            result[key] = value
        return result
    return json.loads(path.read_bytes(), object_pairs_hook=unique)


def write_new(path, value):
    import os  # fsync only; never inspect process environment.
    with path.open('x', encoding='utf-8') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')
        stream.flush()
        os.fsync(stream.fileno())


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()


def pin(root, sha):
    require(git(root, 'rev-parse', 'HEAD') == sha, 'wrong checkout SHA')
    require(not git(root, 'status', '--porcelain'), 'dirty checkout')


def approved_plan(path):
    require(digest(path.read_bytes()) == PLAN_SHA, 'wrong plan SHA')
    return load(path)


def prereg_module(ato):
    spec = importlib.util.spec_from_file_location('immutable_d3_prereg', ato / 'scripts/acceptance/proposal/deepseek-live.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise Stop('public pricing redirect refused')


def fetch_pricing(folder):
    """Unauthenticated, single, fixed public URL; no cookies/proxy/header logging."""
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    request = urllib.request.Request(PRICE_URL, headers={'Accept': 'text/html'})
    with opener.open(request, timeout=30) as response:
        require(response.status == 200 and response.geturl() == PRICE_URL, 'pricing final URL/status')
        raw = response.read(1024 * 1024 + 1)
        require(0 < len(raw) <= 1024 * 1024, 'pricing size')
        final_url = response.geturl()
    timestamp = datetime.now(timezone.utc).isoformat()
    path = folder / 'fresh-pricing.html'
    with path.open('xb') as stream:
        stream.write(raw)
    return path, dict(fetch_timestamp_utc=timestamp, final_url=final_url, response_sha256=digest(raw))


def helper(binary, operation, plan, journal):
    result = subprocess.run([str(binary), operation, str(plan), str(journal)],
                            check=True, capture_output=True, text=True, timeout=30)
    return json.loads(result.stdout)


def verify_binaries(manifest):
    require(manifest['execution_sha'] == ATO_SHA and manifest['api_sha'] == API_SHA, 'binary provenance pins')
    require(set(manifest['binaries']) == {'requester','runtime','worker','budget','project'}, 'binary inventory')
    for item in manifest['binaries'].values():
        path = pathlib.Path(item['path'])
        require(path.is_file() and not path.is_symlink(), 'binary missing or symlink')
        require(digest(path.read_bytes()) == item['sha256'], 'binary hash mismatch')


def check(args, fetch=fetch_pricing):
    """The ordered trace is recorded for review. No journal/credential side effects."""
    trace = []
    plan = approved_plan(args.plan); trace.append('A')
    pin(args.ato, ATO_SHA); trace.append('B')
    pin(args.api, API_SHA); trace.append('C')
    wasm = args.api / 'src/services/runtime_network/wasm/receipt_authority.wasm'
    require(not wasm.is_symlink() and digest(wasm.read_bytes()) == WASM_SHA, 'wrong WASM'); trace.append('D')
    prereg = prereg_module(args.ato)
    prompt = args.ato / plan['prompt']['path']
    require(digest(prompt.read_bytes()) == plan['prompt']['sha256'], 'prompt drift'); trace.append('E')
    require([cell['id'] for cell in plan['cells']] == plan['cell_order'], 'cell order mismatch')
    manifest = load(args.binaries)
    # Binaries are locally built and pinned; recheck hashes at M and every launch.
    configs = {}
    for cell in plan['cells']:
        files = prereg.fixture_files(args.ato, cell['id'])
        require(files == cell['files'], 'fixture drift')
        require(prereg.sha(prereg.json_bytes(files)) == cell['source_manifest_sha256'], 'fixture manifest drift')
        configs[cell['id']], _ = prereg.prepare(args.ato, cell['id'], args.run / 'projections')
    trace.append('F')
    for cell in plan['cells']:
        cfg = configs[cell['id']]
        actual = prereg.project(pathlib.Path(manifest['binaries']['requester']['path']), cfg, args.run/'projections'/cell['id'])
        require(actual == cell['projection'], 'Rust prereg projection drift')
    trace.append('G')
    for cell in plan['cells']:
        projected = load(pathlib.Path(configs[cell['id']]['result']))
        require(projected['source_context_sha256'] == cell['projection']['source_context_sha256'], 'source context drift')
    trace.append('H')
    page, fetched = fetch(args.run)
    # Parser checks exact model/version and fails closed when the official table changes.
    price = prereg.pricing_snapshot(page, fetched['fetch_timestamp_utc'])
    require(fetched['final_url'] == PRICE_URL and fetched['response_sha256'] == digest(page.read_bytes()), 'pricing fetch evidence mismatch')
    require(price['model'] == plan['transport']['model'] and price['model_version'] == 'DeepSeek-V4.1-Flash', 'model unavailable')
    trace.append('I')
    age = (datetime.now(timezone.utc) - datetime.fromisoformat(fetched['fetch_timestamp_utc'].replace('Z','+00:00'))).total_seconds()
    require(0 <= age <= 3600, 'stale price page')
    require(price['input_price_usd_micros_per_million'] <= plan['budget']['input_price'] and
            price['output_price_usd_micros_per_million'] <= plan['budget']['output_price'], 'price increase')
    trace.append('J')
    reservation = helper(manifest['binaries']['budget']['path'], 'check', args.plan, args.journal)
    require(reservation['per_call_usd_micros'] == plan['reservation']['per_call_usd_micros'] and
            reservation['total_usd_micros'] == plan['reservation']['total_usd_micros'] and
            reservation['total_usd_micros'] <= plan['budget']['ceiling_usd_micros'], 'cost journal mismatch')
    trace.append('K')
    require(not args.journal.exists() and not args.journal.is_symlink(), 'existing/corrupt journal: GET-only recovery required')
    trace.append('L')
    verify_binaries(manifest); trace.append('M')
    evidence = dict(trace=trace, plan_sha256=PLAN_SHA, execution_sha=ATO_SHA, api_sha=API_SHA,
                    wasm_sha256=WASM_SHA, pricing={**fetched, **price}, reservation=reservation,
                    binaries=manifest, live_calls=0, key_read=False, execution_ready=False,
                    blocker=CAPTURE_BLOCKER)
    write_new(args.run/'preflight.json', evidence)
    return plan, configs, manifest, evidence


def initialize_budget(args, manifest):
    """Invoked only after all preflight and review gates; never by --preflight."""
    approved_plan(args.plan)
    verify_binaries(manifest)
    return helper(manifest['binaries']['budget']['path'], 'create', args.plan, args.journal)


def require_exact_request_capture():
    # An immutable execution snapshot cannot acquire a hook from controller code.
    # Do not call G0/G1 then discover the G2 evidence gap after spending money.
    raise Stop(CAPTURE_BLOCKER)
