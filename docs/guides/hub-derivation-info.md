# Describing a Capsule Hub run method

`ato hub describe` reads publisher-authored information for one declared
Derivation of one published bundle. Estimates and dependencies are advice;
they never change the bundle, Contract, Derivation, runtime admission or the
verification receipt.

Read the current revision (public; no login required):

```sh
ato hub describe capsules/OWNER/NAME --derivation sha256:DERIVATION
```

To update, sign in as the namespace owner with `ato login`, then write a JSON
file containing the revision returned by the read and the desired fields:

```json
{
  "expected_revision": 0,
  "info": {
    "label": "Build from source",
    "estimated_first_run_seconds": 2700,
    "estimated_cached_run_seconds": 300,
    "estimate_environment": "Apple M1; fresh toolchain cache",
    "dependencies": [
      {"name":"Apple command-line tools","version":null,"provisioning":"host","purpose":"build"},
      {"name":"Rust","version":"1.95.0","provisioning":"managed","purpose":"build"}
    ],
    "notes": "Measured estimates, not a guarantee."
  }
}
```

```sh
ato hub describe capsules/OWNER/NAME@sha256:BUNDLE --derivation sha256:DERIVATION --file info.json
```

Use a pinned bundle digest when editing an earlier read. A tag is resolved
once on each invocation. `--api-url https://staging.api.ato.run` selects the
staging API and its separately stored login. API support must be deployed;
older deployments return an error without modifying anything.

The API path is `PUT /v1/hub/capsules/:owner/:name/versions/:digest/derivations/:derivation/info`;
GET reads the same resource. Durations are integer seconds (1–604800).
Dependencies have provisioning `host`, `managed`, or `download` and purpose
`build`, `run`, or `both`. At most 16 entries are accepted. Missing scalar
fields clear to null; an empty `info` clears all advice. Updates replace the
whole document, advance the revision, and reject stale revisions with 409.
Read again and reconcile edits before retrying. A new bundle has independent
information even when it reuses a DerivationRef.

The Hub detail page displays the same information and provides an owner-only
“Edit run details” form. Saving errors retain the draft; “Reload saved values”
explicitly replaces a stale draft with the saved version.
