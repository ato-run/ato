# runtime-launch-spec-v2

The exact bytes a `RuntimeLaunchSpecV2` takes on the wire for an OCI service
group: an internal backend that owns the only state slot and the only secret,
and a web service that serves the one Surface.

The file is RFC 8785 canonical (`serde_jcs`), so it is also the
cross-language check: `ato-api` builds the same spec, canonicalizes it, and
must reproduce these bytes and their digest.

It contains no secret value and no host path.
