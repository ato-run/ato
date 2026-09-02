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
