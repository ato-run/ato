# oci-alpine-hello

Minimal OCI runtime sample. Runs `alpine:3.21` via the `oci` runtime driver and prints a hello message.

## What this demonstrates

- `[targets.app] runtime = "oci"` — OCI container runtime kind
- `image` field wiring through the compat import → lock resolution pipeline
- No source code needed; the capsule delegates entirely to the container image

## Prerequisites

- Docker Desktop (or compatible Docker Engine) must be running
- Internet access to pull `alpine:3.21` on first run

## Run

```bash
ato run .
```

Expected output:

```
[main] hello from alpine OCI runtime
```

## Notes

The `image` field is pinned to `alpine:3.21` without a digest to keep the sample readable.
Production capsules should pin to a full `image@sha256:...` digest for reproducibility.
