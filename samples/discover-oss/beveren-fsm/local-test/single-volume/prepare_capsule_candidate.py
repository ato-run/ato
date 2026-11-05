#!/usr/bin/env python3
"""Prepare a local-only portable authoring directory; never publish it."""
import argparse
import json
from pathlib import Path
import re
import shutil


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('output', type=Path, help='new output directory')
    parser.add_argument('--image', required=True, help='locally available repository@sha256 digest')
    args = parser.parse_args()
    if not re.fullmatch(r'[a-z0-9][a-z0-9./:_-]*@sha256:[0-9a-f]{64}', args.image):
        raise ValueError('A digest-pinned lowercase OCI image is required')
    here = Path(__file__).resolve().parent
    args.output.mkdir(exist_ok=False)
    for name in ('supervisor.py', 'bootstrap_binding.py', 'wsgi_single.py'):
        shutil.copyfile(here / name, args.output / name)
    manifest = f'''schema = "ato.capsule/2"

[application]
title = "Beveren FSM — Local Candidate"
surface_path = "/"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[binding]]
id = "initial_admin_password"
protocol = "ato.secret@1"
required = true

[[state]]
id = "data"
use = "ato.state.filesystem@1"
mount = "/data"
access = "read-write"

[contract]
mode = "all"

[[contract.observation]]
id = "login"
use = "ato.contract.http@1"
port = "app.http"
path = "/login"
status = 200

[[contract.observation]]
id = "api-ready"
use = "ato.contract.http@1"
port = "app.http"
path = "/api/method/frappe.ping"
status = 200

[[derivation]]
id = "oci-linux-amd64"
use = "ato.oci@1"
argv = ["/ato/workspace/supervisor.py"]
cwd = "."
guest_port = 8000
runtimes = {{ "oci.image" = {json.dumps(args.image)}, "oci.platform" = "linux/amd64", "oci.entrypoint" = "/home/frappe/frappe-bench/env/bin/python", "oci.workspace_mount" = "/ato/workspace", "oci.working_dir" = "/home/frappe/frappe-bench", "oci.memory_bytes" = "2147483648", "oci.cpu_limit_millis" = "2000", "oci.pids_limit" = "256" }}

[derivation.startup]
port = "app.http"
path = "/api/method/frappe.ping"
timeout_ms = 300000
expect_status = 200

[effects]
default = "pure"
'''
    (args.output / 'capsule.toml').write_text(manifest, encoding='utf-8')
    (args.output / 'LOCAL-ONLY.txt').write_text(
        'Synthetic local proof only. This is not a publishable or hosted-ready app.\n', encoding='utf-8'
    )
    print('Prepared local-only candidate; no image was published or imported.')


if __name__ == '__main__':
    main()
