# Isolated Beveren test artifacts

These files record a synthetic local experiment, not a deployable stack. The
database is tmpfs and is lost when its container stops. Do not use real data.
The Compose network is internal and the scheduler is disabled.

The Dockerfile expects a build context with the pinned upstream app in `app/`,
its separately built scheduling assets, and `flit_core-3.12.0-py3-none-any.whl`
(SHA-256 `e7a0304069ea895172e3c7bb703292e992c5d1555dd1233ab7b5621b5b69e62c`).
The image is built with `--network none`. The image itself contains unpatched
source. Generate the candidate patched modules with the scripts in the
parent directory and mount them as shown in `compose.yml` before testing.

The test directory also needs writable `sites2`, `logs2`, `config2`, and
`redis2` directories; `sites2/assets` must point to
`/home/frappe/frappe-bench/assets`. Supply unique synthetic DB and admin
passwords in a private environment file. The `init` service creates a new
site; it is not safe to rerun against an existing database. The scripts run in
the `init` image and communicate with `web` on the internal network. Run
`setup_synthetic.py`, then `workflow.py`, followed by `authz_probe.py`,
`shared_order_probe.py`, `appointment_probe.py`, `order_mapping_probe.py`, `linked_request_probe.py`,
`schedule_probe.py`, and `schedule_workflow_probe.py`; `smoke.py` checks the
login page and built scheduling assets. The probes create synthetic records
and users and assume the first test order and request use the IDs shown in the
scripts. They do not cover the entire Frappe or Beveren permission surface.
