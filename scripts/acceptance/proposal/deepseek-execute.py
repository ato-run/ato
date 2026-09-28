#!/usr/bin/env python3
"""D3-E controller. No model HTTP client or DeepSeek credential access.

The immutable execution pin lacks exact post-claim request capture. Therefore
--execute fails BEFORE journal initialization/key read until re-preregistered.
The control loop and fault proxy are offline-reviewable, not live-authorized.
"""
import argparse
import pathlib
import sys
sys.dont_write_bytecode = True
from execution.preflight import Stop, check, initialize_budget, require_exact_request_capture
from execution.build import build
from execution.runner import execute


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode',choices=['build','preflight','execute'])
    parser.add_argument('--ato',type=pathlib.Path,required=True)
    parser.add_argument('--api',type=pathlib.Path,required=True)
    parser.add_argument('--plan',type=pathlib.Path,required=True)
    parser.add_argument('--binaries',type=pathlib.Path,required=True)
    parser.add_argument('--run',type=pathlib.Path,required=True)
    parser.add_argument('--journal',type=pathlib.Path,required=True)
    args=parser.parse_args()
    args.run.mkdir(parents=True,exist_ok=False)
    if args.mode=='build':
        build(args)
        return
    plan,configs,manifest,evidence=check(args)
    if args.mode=='preflight':
        print('A-M PASS; execution BLOCKED: exact request capture unavailable; live calls=0; key unread')
        return
    require_exact_request_capture()
    initialize_budget(args,manifest)
    execute(args,plan,configs,manifest)


if __name__=='__main__':
    try:
        main()
    except (Stop, ValueError) as error:
        # Only controller-generated classifications, never provider bodies/keys.
        raise SystemExit(str(error)) from None
