# OCI Ingress Router Smoke Test

> **Historical manual test.** This covers the removed provider-centric OCI
> launch path, not the current Capsule lifecycle.

Manual verification for the local path ingress router runtime.

## Prerequisites

- Podman installed and running (`podman --version`)
- Docker Hub accessible (for `nginx:alpine` pull)

## Smoke Recipe

```bash
ATOHOME="$(mktemp -d /tmp/ato-ingress.XXXXXX)"
export ATO_HOME="$ATOHOME"
```

Run the ingress smoke capsule:

```bash
cargo run -p ato-cli -- run samples/recipes/oci-ingress-smoke
```

Watch for output like:

```
🌐 Ingress available at http://127.0.0.1:XXXXX/i/YYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYY/
```

## Verification Steps

### 1. List sessions

In another terminal:

```bash
export ATO_HOME="$ATOHOME"
cargo run -p ato-cli -- ps --all --json | jq .
```

Expected: JSON output includes `"ingress"` block with `primary_url`, `router_port`, `routes`.

### 2. Test root route (web service)

```bash
curl -i http://127.0.0.1:<router-port>/i/<token>/
```

Expected: HTTP 200, nginx welcome page HTML.

### 3. Test alias route (api service)

```bash
curl -i http://127.0.0.1:<router-port>/i/<token>/api/
```

Expected: HTTP 200, nginx welcome page HTML.

### 4. Test unknown alias

```bash
curl -i http://127.0.0.1:<router-port>/i/<token>/unknown/
```

Expected: HTTP 404, body "Not found".

### 5. Test unknown token

```bash
curl -i http://127.0.0.1:<router-port>/i/bad-token/
```

Expected: HTTP 404, body "Not found".

### 6. Cleanup

```bash
cargo run -p ato-cli -- stop --all
```

Verify no managed containers remain:

```bash
podman ps --filter label=io.ato.managed=true
```

Expected: empty list.

## Endpoint Shape

```
http://127.0.0.1:<router-port>/i/<session-token>/      → root route (web service)
http://127.0.0.1:<router-port>/i/<session-token>/api/  → alias route (api service)
```

## Limitations

- No hostname-based routing (v2 deferred)
- No TLS on the ingress endpoint
- No WebSocket support
- No env injection yet (separate PR)
- Path-based apps with absolute asset paths may still fail
- Router runs as a tokio task within `ato run` (not a separate process)
- Response body is buffered (not streamed) — http-body version mismatch
  between axum 0.7 (http-body 1.0) and hyper 0.14 (http-body 0.4); streaming
  is tracked as follow-up work
