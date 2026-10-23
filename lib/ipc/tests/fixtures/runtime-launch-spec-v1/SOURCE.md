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

`native-terminal-oci.json` extends the FastAPI OCI wire fixture with a nonroot
numeric user and an opaque `{derivation_ref, adapter}` native declaration.
Its Terminal catalog is strictly decoded and validated by runtime-attempt;
IPC only checks the JSON object's byte bound. The canonical bytes (without the
file's trailing newline) digest to
`sha256:dd69e6a80b94332365f8290ababc295dd723dbf27c07aa817a7bf55f239688dc`.
This is a cross-language wire fixture, not native execution admission: both
the Runner and ordinary OCI conversion continue to refuse native launch.
