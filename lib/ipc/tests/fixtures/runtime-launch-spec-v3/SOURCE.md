# runtime-launch-spec-v3

The exact bytes a `RuntimeLaunchSpecV3` takes on the wire: the same OCI
service group as `runtime-launch-spec-v2/service-group.json`, with its one
state slot backed by a Runner-local persistent volume instead of a State
Revision.

- `service-group-volume.json` — an existing volume (no seed).
- `service-group-volume-seeded.json` — a volume being provisioned from a seed
  revision.

The files are RFC 8785 canonical (`serde_jcs`), so they are also the
cross-language check: `ato-api` builds the same specs, canonicalizes them, and
must reproduce these bytes and their digests.

They contain no secret value and no host path: a volume is named by
`volume_ref` only.
