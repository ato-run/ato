"""Acceptance evidence checks, not a new Contract evaluator."""
import base64
from .preflight import require, digest, load, helper


def frozen_status(cell, status):
    frozen=cell['projection']['frozen']
    require(status['contract_ref']==frozen['contract_ref'], 'ContractRef drift')
    require(status['search_state']['frozen']==frozen, 'frozen K/source/policy drift')
    attempts=status['search_state']['attempts']
    require(not any(a['status']=='unknown' and not a.get('unknown_resolved') for a in attempts), 'unresolved UNKNOWN')
    return frozen


def known_failure(cell, status):
    """Called before forwarding G2's claim; no fabricated failure is injected."""
    frozen=frozen_status(cell,status)
    require(len(frozen['candidates'])==1, 'G2 registered known D missing')
    known=frozen['candidates'][0]['derivation_ref']
    summaries=[a for a in status['search_state']['attempts'] if a['derivation_ref']==known]
    require(len(summaries)==1, 'G2 exact known attempt missing')
    summary=summaries[0]
    require(summary['status']=='fail' and summary['record']=='finished' and summary['claimed'] is True,
            'G2 known D not actually finished FAIL')
    attempts=[a for a in status['attempts'] if a['attempt_id']==summary['attempt_id']]
    require(len(attempts)==1, 'G2 actual attempt missing')
    attempt=attempts[0]
    require(attempt['derivation_ref']==known and attempt['status']=='fail' and attempt['finished_at'], 'G2 attempt mismatch')
    attestation=attempt['attestation']
    require(attestation['execution_started'] is True and attestation['attempt_record']=='finished' and
            attestation['derivation_ref']==known and attestation['contract_ref']==frozen['contract_ref'], 'G2 execution attestation')
    actual=attempt['formation_attempt']
    require(actual['attempt_id']==summary['attempt_id'] and actual['derivation_ref']==known and
            actual['contract_ref']==frozen['contract_ref'] and actual['failure']['stage']=='verification', 'G2 not actual verification failure')
    receipt=actual['receipt']
    require(receipt['schema']=='ato.contract-verification-receipt/1' and
            receipt['contract_ref']==frozen['contract_ref'] and receipt['derivation_ref']==known and
            receipt['execution']['attempt_id']==summary['attempt_id'] and
            receipt['execution']['realization']=='process' and receipt['fully_satisfied'] is False,
            'G2 failure receipt not bound to actual known-D attempt')
    require(any(o['outcome']=='failed' and o.get('evidence',{}).get('status')==404 for o in receipt['observations']), 'G2 expected actual HTTP failure absent')
    require(summary['failure_code']=='http_status_mismatch', 'G2 wrong failure class')
    return dict(attempt_id=summary['attempt_id'],derivation_ref=known,contract_ref=frozen['contract_ref'],receipt=receipt)


def before_claim(cell, status, claim):
    frozen_status(cell,status)
    point=status['proposal_point']
    require(point and point['claimed'] is False and point['round_seq']==1 and
            point['revision']==claim['revision']==status['search_state']['revision'], 'claim snapshot/revision mismatch')
    require(not status['search_state']['owner_stopped'], 'owner stop')
    require(not any(a['status'] in ('running','pending') for a in status['search_state']['attempts']), 'inflight attempt')
    return known_failure(cell,status) if cell['id']=='G2' else None


def journal_state(path, plan, completed_searches):
    """Read operational evidence only; create/reopen/validate budget stays Rust-owned."""
    raw=path.read_bytes()
    require(raw.endswith(b'\n') and len(raw)<=65536, 'corrupt journal')
    import json
    entries=[json.loads(line) for line in raw.splitlines()]
    require(entries[0]==plan['budget'], 'journal budget drift')
    cells=[entry for entry in entries[1:] if isinstance(entry,str)]
    require(cells==completed_searches, 'journal order/count mismatch or pending reservation')
    require(not any(isinstance(e,dict) and e.get('halt') for e in entries[1:]), 'run STOP recorded')
    responses=[e['response'] for e in entries[1:] if isinstance(e,dict) and 'response' in e]
    require([r['cell'] for r in responses]==cells, 'unresolved provider response')
    for response in responses:
        require(response['finish_reason']=='stop' and response['model_matches'] is True and
                response['input_tokens']<=plan['budget']['input_token_cap'] and
                response['output_tokens']<=plan['budget']['output_token_cap'], 'provider protocol violation')
    return responses


def cell_result(cell, result, responses, loss=False):
    status=result['status']
    frozen=frozen_status(cell,status)
    require(status['status'] not in ('running','unknown'), 'unbounded/unknown terminal state')
    round=status['proposal_round']
    call=round['provider_call']
    # Invalid Proposal JSON is still a successful provider call, followed by
    # invalid_output from the Ato validator. It is ordinary, not a repair trigger.
    require(call['status']=='success' and call.get('error_class') is None, 'provider protocol/infrastructure STOP')
    provenance=call['provenance']
    require(provenance['provider']=='deepseek' and provenance['model']=='deepseek-flash' and
            provenance['prompt_version']=='ato.formation-candidate-producer-prompt/1', 'provider identity drift')
    response=responses[-1]
    require(provenance['usage']==dict(input_tokens=response['input_tokens'],output_tokens=response['output_tokens']), 'usage journal mismatch')
    raw=base64.b64decode(round['raw_output_base64'],validate=True)
    require(len(raw)<=16384 and len(raw)>0 and round['raw_output_digest']=='sha256:'+digest(raw), 'raw output mismatch')
    admitted={o['derivation_ref'] for o in round['outcomes'] if o['status']=='admitted'}
    allowed=admitted | {d['derivation_ref'] for d in frozen['candidates']}
    for attempt in status['attempts']:
        require(attempt['derivation_ref'] in allowed, 'unauthorized execution')
        if attempt.get('attestation'):
            require(attempt['attestation']['effects']=='pure', 'effect expansion')
    receipts=[]
    for route in status['verified_routes']:
        require(route['derivation_ref'] in admitted, 'not an admitted new D')
        for item in route['verifier_receipts']:
            if item['kind']=='contract_verification':
                receipt=item['receipt']
                require(receipt['fully_satisfied'] is True and receipt['contract_ref']==frozen['contract_ref'] and
                        receipt['derivation_ref']==route['derivation_ref'] and receipt['execution']['attempt_id']==route['attempt_id'], 'false verified route')
                receipts.append(receipt)
    if status['verified_routes']:
        require(result['accepted'] and result['receipt_required_checked'] and receipts, 'Requester did not accept actual receipts')
    if cell['id']=='G0':
        passed=bool(admitted) and round['status']=='completed'
    elif cell['id'] in ('G1','G2','G5'):
        passed=bool(receipts and result['accepted']) and (cell['id']!='G5' or loss)
    else:
        passed=True  # Safety/boundedness above, not forced unsupported or efficacy.
    return dict(cell=cell['id'],gate='PASS' if passed else 'FAIL',terminal_classification='ordinary_model_outcome',
                raw_assistant_sha256=digest(raw),raw_assistant_text=raw.decode('utf-8'),
                provider_call=call,validator_outcomes=round['outcomes'],generated_derivation_refs=sorted(admitted),
                attempt_ids=[a['attempt_id'] for a in status['attempts']],receipts=receipts,
                contract_ref=frozen['contract_ref'],runtime_ids=sorted({a['runtime_id'] for a in status['attempts']}))
