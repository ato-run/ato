# OCI profile: `oci.working_dir`, state over an image VOLUME, no anonymous volumes

Status: implemented; offline actual verified on one host. Not merged, not
deployed. Hosted projection: ato-api `feat/oci-working-dir-projection`.

## Change

- Single-container `ato.oci@1` routes may declare `oci.working_dir` (absolute,
  traversal-free). Omitted: `/app`, and existing D bytes are unchanged.
  Validator, CLI, common Adapter and the Hosted v1 projection agree. The
  working directory is independent of `oci.workspace_mount`.
- Service groups keep `/app`; the Hosted runner now checks that explicitly
  because the Adapter no longer does.
- The Adapter refuses an image whose VOLUMEs have no mount at exactly their
  path (workspace mount, a declared `[[state]]` mount or the `/tmp` tmpfs), with
  `oci_volume_unauthorized`, before a container exists. A mount above or below
  the VOLUME path does not count (Docker's own rule is an exact destination).
  Docker would otherwise create an anonymous writable volume outside the
  read-only root, and `rm --force` would leave it on the host.
- No permission is added automatically: the container keeps `--read-only`,
  `--cap-drop=ALL` and `no-new-privileges`; with a writable state mount the
  Adapter's existing `--user` is the state directory's host owner.

## Offline actual (ubuntu-sugamo x86_64, Docker 29.1.3)

Fixture `scripts/acceptance/oci-profile/fixture/`: `WORKDIR /opt/app`,
`VOLUME /opt/app/server-data`, `USER 1000:1000`, `CMD ["python3","server.py"]`
(a relative path). Image built by the 6b-D2 private session:
`ato-source/bbd5ba00d0a7@sha256:9cac7219…`. Each case: pack -> offline export
-> air-gapped run from an empty private store
(`scripts/acceptance/oci-profile/offline.sh`, source `70189cc4`).

| Case | D | Result |
|---|---|---|
| declared: `oci.working_dir=/opt/app` + state `server_data` at the VOLUME; K pins `/whoami` body | `sha256:da65d99c…` | `fully_satisfied=true` (root, whoami satisfied); private-store volumes 0 |
| no_state: working_dir only | `sha256:f7cd35d2…` | refused before start: "OCI image declares VOLUME ["/opt/app/server-data"] that no declared state or workspace mount covers"; volumes 0 |
| legacy: state only, no working_dir (`/app`) | `sha256:9be81ebf…` | container exited with code 2 before observation (relative `server.py` under `/app`) |

`/whoami` body pinned by K: `{"cwd": "/opt/app", "gid": 1000,
"state_writable": true, "uid": 1000}` (`sha256:256950ca…`). The effective
UID/GID 1000:1000 comes from the Adapter's `--user` (host owner of the state
directory, the run user uid 1000) and coincides with the image's
`USER 1000:1000` on this host; a different host owner would change the
effective identity, which this K would then report as unsatisfied.

K (`sha256:778d5380…`) observes HTTP only; this is not a functional claim.

That first run used the earlier "at or above" coverage rule (source
`70189cc4`) and is kept as recorded. The rule was then narrowed to exact
destinations, because a parent mount does not stop Docker from creating the
anonymous volume. Re-run on the exact rule (source `6a5b1952`, `ato` sha256
`34bdfe23068aed51…`): declared `fully_satisfied=true` (root and whoami
satisfied, 0 store volumes); no_state refused with `oci_volume_unauthorized`;
legacy exits under `/app`. Host daemon 68/5/67 unchanged.

## VOLUME coverage on the real daemon (exact destination rule)

`scripts/acceptance/oci-profile/volumes.sh` (same source and binary). Images
from `volume-fixtures/` are built by the 6b-D2 session. While each run is live,
the private daemon's containers are inspected (`.Mounts`); afterwards its
volumes are listed.

| Case | Image VOLUME | Declared mounts | Result | Containers | Volume mounts / store volumes |
|---|---|---|---|---|---|
| exact_state | `/data` | state `/data` | `fully_satisfied=true` | 1 | 0 / 0 |
| exact_wbo_fixture | `/opt/app/server-data` | state at that path | `fully_satisfied=true` | 1 | 0 / 0 |
| exact_tmp | `/tmp` | `/tmp` tmpfs | `fully_satisfied=true` | 1 | 0 / 0 |
| parent_state | `/data/sub` | state `/data` | `oci_volume_unauthorized` | 0 | 0 / 0 |
| tmp_child | `/tmp/cache` | `/tmp` tmpfs | `oci_volume_unauthorized` | 0 | 0 / 0 |
| workspace_child | `/.ato-workspace/x` | workspace `/.ato-workspace` | `oci_volume_unauthorized` | 0 | 0 / 0 |
| child_mount_only | `/data` | state `/data/sub` | `oci_volume_unauthorized` | 0 | 0 / 0 |

Refusals happen before `docker run`: no container was ever created.

## Hosted projection reaching the Adapter

`lib/ipc/tests/fixtures/runtime-launch-spec-v1/portable-oci-working-dir.json`
holds the exact bytes that ato-api's `portableRouteRealization` produces (ato-api
node contract test). `lib/runtime-attempt` parses them and maps them with
`container_oci_spec` to the common Adapter spec:
- `working_dir` is `/opt/app`
- the workspace mount is `/.ato-workspace`
- there is one writable state mount, at the VOLUME path
- `validate_oci_spec` accepts the result

With the fact removed, the spec keeps `/app`. A Hosted Runner launch on Docker
was not executed, because the fixture image is not in a pullable registry. The
Docker behaviour is that of the same Adapter, verified above through the CLI.

## Before production deployment (a gate, not a merge blocker)

Enumerate the existing Hosted OCI apps read-only: image digest,
`Config.Volumes` and declared mounts. List the apps this rule would refuse. Do
not stop running containers or delete existing anonymous volumes, since they
may hold data. Migrate each affected app to an explicit state slot once its
data is preserved, then restart it.
