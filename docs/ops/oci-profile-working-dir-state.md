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
- The Adapter refuses an image whose VOLUMEs are not covered (at or above) by
  the workspace mount, a declared `[[state]]` mount or the `/tmp` tmpfs.
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
