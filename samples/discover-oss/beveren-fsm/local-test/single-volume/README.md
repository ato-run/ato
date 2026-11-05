# Single-volume feasibility experiment

Status: synthetic local experiments on Docker Desktop arm64 and emulated amd64, **not** a
deployable artifact. The `Dockerfile` extends the parent experiment's locally
built `beveren-fsm-local-proof:20261106`, whose image ID was
`sha256:e55374d33a975d5d1bd3757e2e08075db1c6fe08d3ef98aba1099a2e13d84c7f`.
It copies MariaDB 10.6.28 from the pinned official MariaDB image and checks its
dynamic library dependencies. The resulting image still contains the
**unpatched** Beveren source. It is intentionally not published.

In the test, MariaDB ran as the same non-root UID as Frappe, with a read-only
container root, `--skip-networking`, a Unix socket under `/data`, all Linux
capabilities dropped and `no-new-privileges`. A named Docker volume at `/data`
held its data, temporary files, the Frappe bench's `sites`, `config` and `logs`
and its synthetic administrator credential. `FRAPPE_BENCH_ROOT=/data/bench`
pointed Frappe's global locks at that writable bench. The pinned app, Python
environment and assets were read through links to the image. A second Redis
container was reachable only on an internal Docker network; its data was
ephemeral and its queue durability was not tested.

A fresh synthetic site installed Frappe, ERPNext and Beveren; setup created a
test company, customer, service request and order. A private test attachment
was uploaded. After stopping and restarting the database container and
manually restarting the Frappe development server, the same login could read
the request, order and attachment. `persist_probe.py` performs the upload and
verification portions of that test. No real customer data was used.

The same fresh initialization, setup, upload and restart check also passed on
an emulated linux/amd64 image on Docker Desktop. `Dockerfile.amd64` uses the
separately built local base `beveren-fsm-amd64-local-proof:20261106` (image ID
`sha256:84a9a9e9e8b3871448d4be30643536c447a33660fab8407fb5bb9556dc52811e`).
After building that base as described in the parent README, build this test
image and run the proof from the repository root:

```sh
docker build --platform linux/amd64 --network none \
  -f samples/discover-oss/beveren-fsm/local-test/single-volume/Dockerfile.amd64 \
  -t beveren-fsm-single-amd64-proof:20261106 \
  samples/discover-oss/beveren-fsm/local-test/single-volume
samples/discover-oss/beveren-fsm/local-test/single-volume/run-amd64-proof.sh
```

The script creates uniquely named synthetic resources on an internal Docker
network and removes them on success; if it fails or `BEVEREN_KEEP_TEST=1` is
set, it prints their names for inspection. It keeps only generated test
credentials in that test volume, not in the repository or command output.
One fresh patched-image initialization took approximately 81 seconds between
creation of the bench app list and the end of app installation on that
emulated host. This exceeds the existing 60-second service-group readiness
limit, but does not predict native Runner timing.

An additional isolated Gunicorn experiment used `wsgi_single.py` with two
workers and two threads against that site. An administrator could sign in,
list synthetic requests, load the scheduling HTML and its JavaScript, and read
the saved attachment; an anonymous request for that private attachment
returned HTTP 403. The wrapper relies on private Frappe module attributes and
has not been validated as a supported or secure hosted entrypoint. In a
separate synthetic request an untrusted `X-Frappe-Site-Name` header did not
change the selected site.

A separate local amd64 image incorporated the digest-checked list,
appointment, schedule and order patches and the single-site WSGI wrapper.
After mounting the previously created synthetic volume, it passed the
unshared/shared-role checks, order-to-purchase mapping checks, and appointment
creation and rescheduling probes; an anonymous request for the private file
again returned HTTP 403. This verifies only those paths against synthetic data,
not the rest of the application or the hosting boundary.

The local-only patched image context can be reproduced from the pinned source
checkout after the unpatched amd64 test image above is built:

```sh
python3 samples/discover-oss/beveren-fsm/local-test/single-volume/prepare_patched_image.py \
  .tmp/beveren-fsm-source .tmp/beveren-patched-image
docker build --platform linux/amd64 --network none \
  -t beveren-fsm-single-amd64-patched-proof:20261106 .tmp/beveren-patched-image
BEVEREN_TEST_IMAGE=beveren-fsm-single-amd64-patched-proof:20261106 \
  BEVEREN_TEST_PATCHED=1 \
  samples/discover-oss/beveren-fsm/local-test/single-volume/run-amd64-proof.sh
```

The preparation command checks the original file digests and refuses an
existing output directory. The optional extra probes cover only the paths named
above. The resulting image remains private to the local Docker daemon.

`private_bootstrap_probe.py` is a separate synthetic experiment against the
pinned Frappe version: it generated an administrator password and a temporary
database bootstrap password in Python, called Frappe's private `_new_site`
function without putting either secret in process arguments, installed the
three applications, and verified the resulting administrator credential. It
checked that neither synthetic secret appeared in captured output and removed
the temporary database user. This is not a user-facing binding implementation:
it relies on a private Frappe API, uses the test database's privileged local
socket user and does not provide crash-safe cleanup or a credential handoff.

`bootstrap_binding.py` applies the same technique to a local candidate that
reads `ATO_BINDING_INITIAL_ADMIN_PASSWORD` and removes it from its process
environment before installation. On a synthetic amd64 site it installed all
three apps and verified the administrator password without command-line
credentials. Tests also rejected missing or short bindings and an existing
site, and confirmed that no temporary setup user remained after normal exit.
The candidate has not been connected to an Ato binding or tested on a Runner;
its temporary privileged account and partially initialized site can survive
an abrupt process termination, so a supervised recovery design is required.

Using the separate Redis and a one-shot `bench worker --queue short --burst`,
the `enqueue_probe.py` / `synthetic_job.py` fixture executed one synthetic job
and wrote its marker under the site's private files. This exercises a worker,
not queue persistence, restart recovery or sustained load. A due synthetic
`frappe.ping` scheduled-job record was also enqueued using its normal job-type
method, run by a one-shot default worker, and observed as `Complete` in the
site's job log. The periodic scheduler daemon itself was not exercised.

The patched image also passed the scripted proof from an empty synthetic amd64
volume, including setup, restart, private attachment, the restricted/shared
roles and a scheduled appointment; the script then removed its test resources.
In the latest run, the temporary setup account and the privileged `frappe`
socket account were removed before the web server started. The app UID could
not authenticate as `frappe` or `root`, while the site-scoped database account
had no global privileges or grant option. The same restart and functional
checks passed using that site account.

An additional local image adds Redis to the patched image and uses its AOF on
the same volume. `supervisor.py` is a local-only candidate that starts Redis,
MariaDB, Gunicorn, workers and the scheduler process, consumes an initial
administrator binding only on first initialization, removes database setup
users before serving, and refuses concurrent use of the same volume. In an
isolated synthetic test it served the app, ran a worker job, retained a private
attachment and Redis value across a normal restart without the initial binding,
and refused automatic restart after a forced stop. The scheduler process runs,
but the synthetic site's scheduler remained disabled; periodic execution has
not been established. The candidate deliberately has no automated recovery
for an interrupted initialization or forced stop.
The synthetic lifecycle proof also passed with a 2 GiB memory limit, two CPUs
and a 256-process limit on the emulated host. These limits and timings still
need verification on the selected hosted Runner.

After building the patched image above, the local image and proof can be run:

```sh
docker build --platform linux/amd64 --network none \
  -f samples/discover-oss/beveren-fsm/local-test/single-volume/Dockerfile.redis \
  -t beveren-fsm-single-amd64-redis-proof:20261106 \
  samples/discover-oss/beveren-fsm/local-test/single-volume
samples/discover-oss/beveren-fsm/local-test/single-volume/run-supervisor-proof.sh
```

The proof supplies only a generated synthetic secret, uses no network, and
removes its resources after success. It intentionally sets `CI` on the test
container to check that the supervisor does not propagate Frappe's test mode
to its background workers.

`prepare_capsule_candidate.py` prepares a separate authoring directory with
only the supervisor, bootstrap and WSGI scripts, a required secret binding, a
single persistent state slot, and login and API readiness observations. It
requires a digest-pinned image reference and refuses an existing output
directory. The current locally built image is not available from a registry;
this is a local-only candidate, not a hosted import or distribution. Parsing
the authored TOML is not proof that a Capsule has been packed, admitted or
executed by an Ato Runner.

This result does not establish a stg Runner-compatible package. The manually
started proof used a development server and ephemeral Redis; the supervisor
candidate remains local and does not establish scheduled execution or a safe
production credential handoff. It has no crash recovery or complete backup,
and no portable Capsule or hosted receipt. Initialization still gives
the app UID a privileged database socket account until setup finishes; failure
handling and recovery for that window need security review. The base image,
dependency notices, source distribution and the remaining application
permissions must also be reviewed before any exposure. Never use this
experiment with real data.
