#!/usr/bin/env python3
"""Legacy owner-side delivery for one saved shared reasoning exchange.

This script never starts a source process, grants authority, or fabricates a
provider API call. The product driver validates/compiles inspection, proposal
and decline. New agents use `ato form-session` without owner filesystem access.
"""
import argparse
import hashlib
import json
import os
import tempfile
from pathlib import Path


def publish(path, data):
    """Same-directory atomic publication; never replace a prior response."""
    fd, name = tempfile.mkstemp(prefix='.response-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        try:
            os.link(name, path)
        except FileExistsError:
            if path.read_bytes() != data:
                raise ValueError('session response conflict') from None
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        os.unlink(name)


def strict_json(raw):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError('duplicate JSON field')
            result[key] = value
        return result
    return json.loads(raw, object_pairs_hook=unique)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--input', required=True, type=Path)
    parser.add_argument('--proposal', '--output', dest='proposal', required=True, type=Path)
    args = parser.parse_args()
    raw = args.input.read_bytes()
    if len(raw) > 65536:
        raise ValueError('input bound exceeded')
    context = strict_json(raw)
    if context['schema'] != 'ato.formation-reasoning-input/1':
        raise ValueError('unsupported input schema')
    proposal_raw = args.proposal.read_bytes()
    if len(proposal_raw) > 16384:
        raise ValueError('proposal bound exceeded')
    proposal = strict_json(proposal_raw)
    if proposal['schema'] != 'ato.formation-proposal/1' or len(proposal['proposals']) != 1:
        raise ValueError('one typed proposal required')
    response = {'schema': 'ato.formation-session-response/1',
                'input_sha256': 'sha256:' + hashlib.sha256(raw).hexdigest(),
                'exchange_id': context['call_id'],
                'output': proposal}
    if not args.input.name.endswith('.input.json'):
        raise ValueError('input filename does not identify an exchange')
    target = args.input.with_name(args.input.name[:-len('.input.json')] + '.response.json')
    publish(target, (json.dumps(response, sort_keys=True, separators=(',', ':'))+'\n').encode())
    print(json.dumps({'response': str(target), 'input_sha256': response['input_sha256'],
                      'response_sha256': hashlib.sha256(target.read_bytes()).hexdigest(),
                      'API_call': False}))


if __name__ == '__main__':
    main()
