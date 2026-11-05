#!/usr/bin/env python3
"""Prepare a new, local-only patched image context from pinned source files."""

import argparse
from pathlib import Path
import shutil
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('upstream', type=Path, help='pinned Beveren source root')
    parser.add_argument('output', type=Path, help='new output directory; never overwritten')
    args = parser.parse_args()
    here = Path(__file__).resolve().parent
    root = here.parents[1]
    api = args.upstream / 'beveren_fsm/field_service_management/api'
    order = args.upstream / 'beveren_fsm/field_service_management/doctype/service_order/service_order.py'
    args.output.mkdir(exist_ok=False)
    commands = (
        ('prepare_list_permission_fix.py', api, args.output / 'api'),
        ('prepare_appointment_permission_fix.py', api / 'service_appointment.py', args.output / 'api/service_appointment.py'),
        ('prepare_schedule_permission_fix.py', api / 'schedule.py', args.output / 'api/schedule.py'),
        ('prepare_movement_permission_fix.py', order, args.output / 'service_order.py'),
    )
    for script, source, target in commands:
        subprocess.run([sys.executable, str(root / script), str(source), str(target)], check=True)
    shutil.copyfile(here / 'Dockerfile.patched', args.output / 'Dockerfile')
    shutil.copyfile(here / 'wsgi_single.py', args.output / 'wsgi_single.py')
    print('Prepared local-only image context; do not publish without a full review.')


if __name__ == '__main__':
    main()
