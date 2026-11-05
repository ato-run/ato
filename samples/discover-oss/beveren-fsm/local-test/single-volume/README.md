# Single-volume feasibility experiment

Status: synthetic local experiment on Docker Desktop arm64, **not** a
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

An additional isolated Gunicorn experiment used `wsgi_single.py` with two
workers and two threads against that site. An administrator could sign in,
list synthetic requests, load the scheduling HTML and its JavaScript, and read
the saved attachment; an anonymous request for that private attachment
returned HTTP 403. The wrapper relies on private Frappe module attributes and
has not been validated as a supported or secure hosted entrypoint.

This result does not establish a stg Runner-compatible package. The proof used
an interactive development server, did not supervise its processes or exercise
the scheduler and workers, and did not make Redis durable. It has no safe
production initialization or credential handoff, no crash recovery or complete
backup, and no portable Capsule or hosted receipt. The socket-authenticated
database administrator shares a UID and process boundary with the app; that
arrangement needs security review. The base image, dependency notices, source
distribution and the remaining application permissions must also be reviewed
before any exposure. Never use this experiment with real data.
