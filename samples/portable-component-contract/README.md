# Component Contract fixture

Native Linux Docker, with the pinned Python image already available, is required.
The two unprivileged containers use an internal network. Only `app.http` is
forwarded to host loopback. `backend.http` is observed on its container address.
K requires the backend health body, frontend health body and the result of an
actual frontend-to-backend request. All three responses have fixed body hashes.

Pack and run through the normal CLI, using a fresh test environment:

```sh
scripts/ato-test-shell.sh /path/to/ato pack samples/portable-component-contract -o .tmp/components.capsule
scripts/ato-test-shell.sh /path/to/ato run .tmp/components.capsule --no-open --verification-receipt .tmp/components.receipt.json
```

For the negative case, copy the source into a private test directory and change
only the backend service's `FAIL_HEALTH` to `1`. Repack with the same K and run it.
The frontend still answers HTTP 200 at `/`, but the whole Run must fail. Neither
K nor its expected response digests may be recaptured from that failed run.
