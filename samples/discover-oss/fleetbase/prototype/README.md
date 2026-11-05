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
The console's entrypoint requires `ATO_BINDING_CONSOLE_ORIGIN`; the API
supervisor separately requires `ATO_BINDING_APP_ORIGIN`. Both must contain the
same canonical origin. A service-group Binding may be delivered to only one
service, so one shared Binding cannot supply both values. Automatic, consistent
delivery of the hosted Instance's actual origin has a proposed implementation
in draft API PR https://github.com/ato-run/ato-api/pull/1136, but is not yet
deployed or verified on staging; a deployable candidate must do that without
embedding a physical
origin in the Contract or Derivation. Nginx sets the upstream Host, scheme and port from the
Binding and removes client-supplied forwarding headers. It accepts an internal
Host so Ato's readiness and Contract observations can reach the application;
the hosted ingress still needs to enforce its own public-host boundary.
`console/run-proxy-proof.sh` verified this behavior with synthetic HTTP and HTTPS
origins, forged headers and invalid origin values on an isolated Docker network.
It deliberately does not forward the original client IP. Upload limits, the
generated instance origin and trusted client IP handling need hosted validation.
The supervisor restricts the socket to the configured
application hostname and port, forces authenticated `enforce` mode, and disables
upstream polling that could override those settings from the console. Browser
origin filtering was tested locally; hosted proxy behavior remains unverified.

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
folder as context. `api/Dockerfile.amd64` pins the corresponding x86_64 image
digests and library paths; the other API Dockerfile targets arm64. Extract the
original `index.html`, `assets/vendor.js` and
`assets/console.js` from the pinned console image into `console/`, run
`python3 console/prepare.py`, and build `console/Dockerfile` with that folder
as context. Use `console/Dockerfile.amd64` and
`breach_proxy/Dockerfile.amd64` for x86_64: their base image digests are the
x86_64 counterparts in the same official multi-platform indexes as the arm64
versions. Console assets come
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

On 2026-11-06, `run-socket-roundtrip.sh` exercised the local arm64 prototype
on an internal Docker network with a newly initialized state volume, generated
synthetic credentials and a test-only mock breach checker. Through the console
nginx proxy, it created two accounts in separate synthetic organizations, obtained a real API-minted socket
token, verified the allowed and a disallowed websocket Origin, and checked
denial of an anonymous subscription and a subscription to the other organization's user channel.
The authenticated client subscribed to its own API-authorized test channel;
the API's signed publish reached that client and ignored an attempted channel
override. This exercised `socketcluster-client` in Node, not a rendered browser
or a hosted Runner, and did not use real users or business data. The script
uses `volume-nocopy`: Docker's default volume population copies the image's
`/data/caddy` into an otherwise new volume, causing the supervisor to reject
the nonempty state as designed. The proof removes its own containers, network
and synthetic volume on exit.

The API image's Docker health check now probes the running onboarding-status
API instead of the upstream image's unused Caddy admin port. After the full
synthetic workflow, Docker reported the API container healthy on both arm64
and x86_64. Docker health is only an operational signal: it does not replace
the Ato Runner's readiness check or Contract receipt.

The proxy and socket round-trip proofs were also built and passed on native
Linux x86_64 on 2026-11-06 using the amd64 Dockerfiles. Their transferred build
and probe inputs were checked by SHA-256 and file mode before building. The
test-only mock breach service, no-egress network, synthetic credentials and
limitations above apply equally to this proof. This is a host-local test, not
a hosted Ato Runner receipt or a staging publication.

On 2026-11-06, the local supervisor was extended to hold an exclusive advisory
lock on the mounted state directory from before initialization until all its
children have stopped. This provides a coordination primitive for a future
offline backup or recovery tool; it does not implement either. All processes
that access the state would have to take the same lock, and the lock does not
replace Runner fencing or establish cross-host filesystem semantics. The
existing unclean-shutdown marker still prevents an automatic restart after a
forced stop.

The revised arm64 local image passed the complete synthetic socket round-trip
above. A second supervisor in another container was refused while the first
held the same Docker volume, and a graceful stop released the lock so the
original container restarted and became healthy again. This checks the local
Docker volume behavior, not a hosted Runner, crash recovery, or a consistent
backup. The lock's unit tests additionally cover a competing process, release,
missing directories and symlink rejection. For the proof, the local
`ato-local-proof/fleetbase-combined:state-lock` image used the earlier
`socket-roundtrip` image with the current `supervisor.py` and `state_lock.py`
copied into `/opt`; a fresh build from the Dockerfiles also copies the lock
module. The proof script accepts another locally built compatible image via
`FLEETBASE_SOCKET_PROOF_APP_IMAGE`.

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

`provenance/verify_console_packages.py` additionally checks the pinned console
`pnpm-lock.yaml` and the archive SHA-256 and lockfile SHA-512 integrity values
for `@fleetbase/ember-core` 0.3.26, `@fleetbase/fleetops-data` 0.2.3 and
`@fleetbase/fleetops-engine` 0.6.75. It checks the package identities and
license-file hashes without extracting the archives; `console-packages.json`
records those results. All three archives declare AGPL-3.0-or-later and include
source-like files and a license file. This establishes the identity of these
three archives against the pinned source lock, not that the released console
image was reproducibly built from them. It does not cover the console's other
JavaScript dependencies or establish a complete corresponding-source bundle.

```sh
PYTHONDONTWRITEBYTECODE=1 python3 samples/discover-oss/fleetbase/prototype/provenance/verify_console_packages.py \
  .tmp/fleetbase-upstream/console/pnpm-lock.yaml \
  .tmp/fleetbase-ember/fleetbase-ember-core-0.3.26.tgz \
  .tmp/fleetbase-fleetops/fleetbase-fleetops-data-0.2.3.tgz \
  .tmp/fleetbase-fleetops/fleetbase-fleetops-engine-0.6.75.tgz \
  > .tmp/fleetbase-console-packages.json
cmp .tmp/fleetbase-console-packages.json samples/discover-oss/fleetbase/prototype/provenance/console-packages.json
```

## Pinned Fleetbase source trees

The upstream v0.7.75 annotated tag resolves to commit
`67b2db64c7490599aaa6c799b26d5e4de897c25c` and tree
`06657c9a96f9c0a286c2b2ebaef4774492305547`. Its source archive contains all
972 regular blobs in that tree, while its 14 Git submodule references require
separate archives. `provenance/fleetbase-source-trees.json` records each pinned
submodule commit, tree, archive digest and file count. The root and all 14
submodule archives were compared against their Git trees: 15,191 regular files
matched by Git blob digest and executable mode. The tree and tag evidence was
read from the upstream GitHub API; no customer data was used.
Each of these 15 source archives contains the same root `LICENSE.md`, whose
recorded SHA-256 identifies the GNU Affero General Public License version 3
text. This records the upstream files; it does not establish the license or
notice obligations of all bundled third-party components.

Four GitHub-generated archives omit files marked `export-ignore`: core-api
omits 251, fleetops 680, registry-bridge 4 and storefront 127. For those four,
`provenance/archive_git_tree.py` produces a complete, deterministic archive
from the pinned Git objects after comparing the tree with the GitHub tree. The
resulting archives were independently checked and rebuilt byte-for-byte. The
other ten upstream archives contained all blobs in their pinned trees.
`provenance/verify_git_tree_archive.py` checks Git tree identity and archive
contents without extracting the archives. The local archives live under
`.tmp/fleetbase-submodule-sources/`; they are not committed or published.

The submodule commits are the ones recorded by the v0.7.75 source tree; they
are not necessarily the same revisions as separately published npm packages.
This evidence does not prove the release images were built from these trees,
and it does not include every third-party dependency or constitute a complete
corresponding-source distribution. Those checks, notice coverage and hosted
acceptance remain open.

## API image source comparison

On 2026-11-06, `provenance/compare_api_source.py` compared the arm64 API
image's installed files with the verified source archives above. The root API
comparison excludes `vendor`, `.composer` and mutable `storage`: 105 source
files matched byte-for-byte, two Git metadata files were absent from the image,
and four image files had no root-source counterpart (`.fleetbase-id`,
`bootstrap/cache/packages.php`, `bootstrap/cache/services.php` and `crontab`).
The image's `crontab` matches `docker/crontab` elsewhere in the pinned root
source archive. The other three have no byte-identical regular file in that
archive and are not independently attributed by this comparison.

For all 12 installed Fleetbase-named Composer packages, all 11,192 installed
regular files matched their pinned complete source archives; 1,070 additional
source files were absent from the installed packages. Seven package commits
match the release submodule pins. The other five are recorded in
`provenance/extra-source-trees.json`: their commit/tree identities were read
from the upstream GitHub API, and the archives were checked against the entire
Git tree with `verify_git_tree_archive.py`. The `laravel-twilio` codeload
archive omitted eight export-ignored files, so its complete archive was
reconstructed from the pinned Git objects using `archive_git_tree.py` and
independently checked against the tree. No installed symlinks or differing shared files were
observed in these paths. `provenance/api-source-comparison.json` records the
individual paths and archive hashes. The comparison detects altered shared
files, rejects archive digest mismatches and unsafe entries, and reports source
and installed files that do not have counterparts. It compares bytes only; it
does not verify modes, other Composer packages, operating-system packages,
container build steps, generated files or corresponding source for the console
and socket images. The image digest is selected by the caller, not verified
from inside the container. This evidence does not make the candidate ready for
distribution or deployment.

From the repository root, with the root and submodule archives described above
present under `.tmp`, reproduce the comparison with the pinned image:

```sh
docker run --rm --network none --read-only \
  --mount type=bind,src="$PWD/samples/discover-oss/fleetbase/prototype/provenance/compare_api_source.py",dst=/compare.py,readonly \
  --mount type=bind,src="$PWD/samples/discover-oss/fleetbase/prototype/provenance/fleetbase-source-trees.json",dst=/input/fleetbase-source-trees.json,readonly \
  --mount type=bind,src="$PWD/samples/discover-oss/fleetbase/prototype/provenance/extra-source-trees.json",dst=/input/extra-source-trees.json,readonly \
  --mount type=bind,src="$PWD/samples/discover-oss/fleetbase/prototype/provenance/composer-inventory.json",dst=/input/composer-inventory.json,readonly \
  --mount type=bind,src="$PWD/.tmp/fleetbase-v0.7.75-source.tar.gz",dst=/input/root.tar.gz,readonly \
  --mount type=bind,src="$PWD/.tmp/fleetbase-submodule-sources",dst=/input/submodules,readonly \
  --mount type=bind,src="$PWD/.tmp/fleetbase-extra-sources",dst=/input/extra-sources,readonly \
  --entrypoint python3 \
  fleetbase/fleetbase-api@sha256:ccd1f9d4ee93625edd9f9aa446e8655cbc2ffbe7c249387bdf52169967933572 \
  /compare.py > .tmp/fleetbase-api-source-comparison.json
cmp .tmp/fleetbase-api-source-comparison.json samples/discover-oss/fleetbase/prototype/provenance/api-source-comparison.json
```
