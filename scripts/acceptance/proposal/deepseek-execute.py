#!/usr/bin/env python3
"""D3-E controller. No model HTTP client or DeepSeek credential access.

Consumes the immutable v2 plan and execution pin. The controller never reads a
provider credential; only the requester injector receives a private broker socket.
"""
import argparse
import pathlib
import sys
sys.dont_write_bytecode = True
from execution.preflight import Stop, check, initialize_budget
from execution.build import build
from execution.runner import execute, execution_platform


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode',choices=['build','preflight','execute'])
    parser.add_argument('--ato',type=pathlib.Path,required=True)
    parser.add_argument('--api',type=pathlib.Path,required=True)
    parser.add_argument('--plan',type=pathlib.Path,required=True)
    parser.add_argument('--binaries',type=pathlib.Path,required=True)
    parser.add_argument('--run',type=pathlib.Path,required=True)
    parser.add_argument('--journal',type=pathlib.Path,required=True)
    parser.add_argument('--credential-socket',type=pathlib.Path,help='private requester-only injector socket; no credential bytes/fd enter controller')
    args=parser.parse_args()
    if args.credential_socket is not None and args.mode!='execute':
        raise Stop('credential socket allowed only after preflight, in execute mode')
    args.run.mkdir(parents=True,exist_ok=False)
    if args.mode=='build':
        build(args)
        return
    plan,configs,manifest,evidence=check(args)
    if args.mode=='preflight':
        print('A-M + v2 evidence contract PASS; live calls=0; key unread')
        return
    if args.credential_socket is None:
        raise Stop('live execution requires the separately authorized requester injector')
    execution_platform()
    initialize_budget(args,manifest)
    execute(args,plan,configs,manifest)


if __name__=='__main__':
    try:
        main()
    except (Stop, ValueError) as error:
        # Only controller-generated classifications, never provider bodies/keys.
        raise SystemExit(str(error)) from None
