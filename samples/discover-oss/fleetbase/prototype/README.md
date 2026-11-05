# Fleetbase local prototype

Status: experimental local proof; **not a staging or production artifact**.
See the parent README for the upstream release and initial proof. This directory
contains the scripts used to investigate whether Fleetbase can fit a portable
OCI service group; it does not include a distributable image or a capsule.

The pinned Fleetbase v0.7.75 images combine the API, MySQL 8.0.46, Redis 8.10.3,
workers, scheduler and authenticated socket service in one process-supervised
container. The database, Redis AOF and application files share one state mount.
MySQL uses a Unix socket and Redis binds to loopback. The supervisor creates
separate random application, database and socket credentials in the state mount;
its database bootstrap credential is not retained. An unclean shutdown leaves
a marker and prevents automatic restart. An interrupted initialization also
fails closed. This intentionally has no automated recovery or backup procedure.
The processes run as the same UID and isolation between them is not a security
boundary. This is not a hardened Fleetbase deployment.

A separate non-root console image serves patched release assets. `prepare.py`
verifies the original asset digests and generates the patched filenames and SRI.
It makes external Fleetbase hosted routing opt-in, uses the current origin for
the API and socket, and fixes the Fleet-Ops vehicle `online` default. The
corresponding source changes for onboarding, Ember Core and the vehicle model
are in the adjacent patches; patches without context require `git apply
--unidiff-zero`. The socket bundle substitution is recorded in `prepare.py`. Source, build provenance and notices for **all** included Fleetbase
plugins and dependencies must be assembled and reviewed before distribution.
The local nginx configuration trusts an incoming forwarded-proto header and is
not suitable for hosted use without a verified trusted-proxy boundary. Upload
limits, the forwarded-proto trust boundary and the generated instance origin
also need hosted validation. The supervisor restricts the socket to the configured
application hostname and port, forces authenticated `enforce` mode, and disables
upstream polling that could override those settings from the console. Browser
origin filtering was tested locally; hosted proxy and authenticated end-to-end
websocket behavior remain unverified.

The upstream Laravel compromised-password check fails open on network errors.
`api/patch_breach_verifier.py` checks the upstream file digest and generates a
fail-closed replacement that talks only to the private `breachcheck` service.
The adjacent patch records the change. The `breach_proxy` service is offline by
default. With an explicitly granted Ato TCP egress binding it sends the first
five SHA-1 characters of a password hash to `api.pwnedpasswords.com` over
certificate-verified TLS; it never sends the full password or full hash. This
external transmission requires an informed user choice. No production endpoint
or target IP is preconfigured. If the grant is absent or verification fails,
registration returns an error rather than silently skipping the check. The
relay tests use a local synthetic TLS/SOCKS fixture; they do not establish that
an actual hosted network grant works.

To reproduce the local build, extract the pinned verifier from the matching
upstream API image into `api/NotPwnedVerifier.php`, run
`python3 api/patch_breach_verifier.py`, and build `api/Dockerfile` with that
folder as context. Extract the original `index.html`, `assets/vendor.js` and
`assets/console.js` from the pinned console image into `console/`, run
`python3 console/prepare.py`, and build `console/Dockerfile` with that folder
as context. These Dockerfiles pin the arm64 runtime images; console assets come
from the upstream amd64 image. The generated inputs and images are intentionally
not checked in. Build and run only in an isolated local environment with
synthetic data. No secrets or real customer evidence belong in the build context.

Local tests covered fresh initialization, anonymous API rejection, creation of
a synthetic account and vehicle with a **test-only mock** breach checker, and
retention of synthetic database, attachment and Redis data after a clean stop
and restart. The same database/file/Redis restart checks also passed on a locally
emulated x86_64 build; a fresh x86_64 initialization took about 147 seconds. They do not establish full Fleetbase functionality or safety.
The initial local initialization exceeded the current hosted 60-second group
readiness limit. The candidate extension is draft PR
https://github.com/ato-run/ato-api/pull/1133; it has not been deployed to stg.
An actual compatible Runner, storage crash recovery, hosted network binding,
source/notice distribution, user credential handoff, staging acceptance and
Discover publication all remain unverified or incomplete.

Relay tests: `PYTHONDONTWRITEBYTECODE=1 TMPDIR="$PWD/.tmp" python3 -m unittest
discover -s samples/discover-oss/fleetbase/prototype/breach_proxy -p 'test_*.py'
-v` from the repository root.

## Composer source and notice inventory

`provenance/composer_inventory.py` reads the API image's installed Composer
metadata and lock file only after verifying their SHA-256 digests. It rejects
unmatched versions or source references, invalid package paths, unpinned Git
references and unsafe notice files. `composer-inventory.json` records the
243 installed packages, their declared license metadata, source references,
and the hashes of top-level license and notice files actually present in the
pinned API image. Re-running against that image produced the same inventory.
The lock and source references are metadata: they do **not** establish that
installed code is byte-identical to those source revisions.

Five installed packages do not include a top-level notice in that image.
`supplemental-notices.json` records the three notice texts obtained from the
exact commits of their upstream repositories. The pinned `phrity/net-stream`
and `phrity/net-uri` repositories contain no license or notice file in their
source trees; both declare MIT in Composer metadata, but no missing copyright
or license text has been invented. `webit/eval-math` contains a license file
but does not declare a license in Composer metadata. These items need further
review. The inventory covers only installed Composer packages: it does not
cover the API's own code, OS and language runtime packages, the console,
JavaScript dependencies, Fleet-Ops, MySQL, Redis, sockets or the auxiliary
service. It is not a complete notice package or a legal determination.

To reproduce the image inventory from the repository root:

```sh
docker run --rm --network none --read-only --user 65532:65532 \
  --mount type=bind,src="$PWD/samples/discover-oss/fleetbase/prototype/provenance/composer_inventory.py",dst=/inventory.py,readonly \
  --entrypoint python3 \
  fleetbase/fleetbase-api@sha256:ccd1f9d4ee93625edd9f9aa446e8655cbc2ffbe7c249387bdf52169967933572 \
  /inventory.py > .tmp/fleetbase-composer-inventory.json
cmp .tmp/fleetbase-composer-inventory.json samples/discover-oss/fleetbase/prototype/provenance/composer-inventory.json
PYTHONDONTWRITEBYTECODE=1 TMPDIR="$PWD/.tmp" python3 -m unittest discover -s samples/discover-oss/fleetbase/prototype/provenance -p 'test_*.py' -v
```
