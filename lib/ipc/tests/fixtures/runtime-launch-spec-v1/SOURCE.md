# runtime-launch-spec-v1

The exact bytes a `RuntimeLaunchSpecV1` takes on the wire, for the P3/P5
acceptance fixture (FastAPI + SQLite) in both realizations.

`fastapi-process.json` and `fastapi-oci.json` differ ONLY in the `realization`
arm. That is the contract's central claim, and these two files are what makes
it falsifiable: state, endpoints, readiness and lifecycle must stay identical
across realizations or Process and OCI have started to mean different things.

Both are RFC 8785 canonical (`serde_jcs`), so they double as the
cross-language check: `ato-api` generates a spec, canonicalizes it, and must
reproduce these bytes.

Neither contains a secret value or a host path, and neither ever may — that is
what makes the digest safe to persist on a Run receipt.

`portable-oci-working-dir.json` is the Hosted projection of a portable
single-container OCI route that declares `oci.working_dir = /opt/app`, a
workspace mount at `/.ato-workspace` and a state slot at the image VOLUME
`/opt/app/server-data`. ato-api's `portableRouteRealization` reproduces it
byte for byte; the Runner maps it onto the common OCI Adapter.
