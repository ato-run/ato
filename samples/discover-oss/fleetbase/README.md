# Fleetbase staging feasibility

Status: local proof only; not yet imported or published on staging. Fleetbase
is distinct from the Puter Fleet preview.

Upstream Fleetbase v0.7.75 resolves to commit
`67b2db64c7490599aaa6c799b26d5e4de897c25c` (AGPL-3.0-or-later). The
upstream release packages Fleet-Ops as separate dependencies; their exact
source and notices must be provided alongside any distribution.

On 2026-11-05 an isolated local Docker test used the upstream API image
`fleetbase/fleetbase-api@sha256:ccd1f9d4ee93625edd9f9aa446e8655cbc2ffbe7c249387bdf52169967933572`
(linux/arm64), console image
`fleetbase/fleetbase-console@sha256:2212ecf41247d0cbc9aef4fa7f9d790943e95392d47422ec68c5164b8f4a2286`
(linux/amd64), and socket image
`fleetbase/fleetbase-socket@sha256:2becf68e5781214797a5fea0c66c0cc0e0caf0df632c95edf12bb63da1017bff`
(linux/arm64), together with MySQL 8.0.46 and Redis 8.2.1. The isolated
network denied external egress; local-only proxies exposed the console and API
on loopback. Test-only DB, Laravel and socket keys were freshly generated,
kept outside the source tree, and socket authentication was enabled.

The upstream `deploy.sh` completed with exit status zero and populated a fresh
database. The API root and onboarding-status endpoint returned 200; an
unauthenticated vehicle request returned 401. Socket health returned 200.
The console reached the onboarding form after a **local-test-only** reverse
proxy and two JavaScript fallback substitutions: the production console image
has runtime config disabled and its blank-API-host fallback omits a non-default
port. The test updated the script's integrity hashes as well. No account was
created, and no login, vehicle workflow, persistence recovery, queue, schedule,
websocket, or attachment workflow was verified.

The onboarding form selects Fleetbase's hosted routing service by default.
Submitting that option can send registration and route information externally;
no form was submitted during this test. A hosted integration needs an explicit
user choice and an appropriate network binding before using any external
service. Do not represent this local proof as a deployable production image.

The current portable OCI service-group profile admits at most four services
and one persistent filesystem state slot visible to exactly one service. The
upstream compose has separate MySQL and application storage as well as Redis,
API, worker, scheduler, socket and console services. A reliable staging route
must preserve both database and application files, supervise the required
processes, preserve authentication, and prove stop/restart and backup behavior
within the profile or use a separately reviewed general runtime extension.
Building the necessary image, connecting a suitable runner, including complete
source/notices, and an actual staging import and acceptance remain outstanding.
