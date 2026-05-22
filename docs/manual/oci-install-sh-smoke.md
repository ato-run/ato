# Manual smoke test: `ato run . --oci-install-sh`

This guide walks through using Ato to extract `docker run` intent from an
`install.sh` script and run the resulting service graph via Podman.

## Prerequisites

- Podman installed and ready (rootless preferred on macOS/Linux)
- On macOS: `podman machine start` must complete before running
- `ato` CLI built from `feat/oci-provider-model` branch (or later)

Verify Podman is available:

```sh
podman version
podman system info | grep rootless
```

## Sample Blinko-style `install.sh`

Create a test directory with a minimal install script:

```sh
mkdir -p /tmp/blinko-test && cd /tmp/blinko-test
cat > install.sh << 'EOF'
#!/bin/bash

docker network create blinko-network

docker run -d \
  --name blinko-postgres \
  --network blinko-network \
  -e POSTGRES_USER=postgres \
  -e POSTGRES_PASSWORD=mysecretpassword \
  -e POSTGRES_DB=blinko \
  -v pgdata:/var/lib/postgresql/data \
  postgres:14

docker run -d \
  --name blinko-website \
  --network blinko-network \
  -p 1111:1111 \
  -e NODE_ENV=production \
  -e DATABASE_URL=postgresql://postgres:mysecretpassword@blinko-postgres:5432/blinko \
  -e NEXTAUTH_SECRET=my_ultra_secure_nextauth_secret \
  --restart always \
  blinkospace/blinko:latest
EOF
chmod +x install.sh
```

## First run — resolves images and writes lock

```sh
ato run /tmp/blinko-test --oci-install-sh
```

Expected diagnostics:

```
[oci-install-sh] Selected script: /tmp/blinko-test/install.sh
[oci-install-sh] Extracted docker networks: blinko-network (source metadata only)
[oci-install-sh] Extracted services: blinko-postgres, blinko-website
[oci-install-sh] Warning: --restart always ignored (Ato session owns lifecycle)
[oci-install-sh] Warning: unsafe default detected for POSTGRES_PASSWORD → generated secret
[oci-install-sh] Warning: unsafe default detected for NEXTAUTH_SECRET → generated secret
[oci-install-sh] Warning: DATABASE_URL rewritten to use alias 'blinko-postgres'
[oci] Resolving images (no lock found, running fresh resolution)...
[oci] postgres:14  → sha256:<digest> (linux/arm64, podman-rootless-v1)
[oci] blinkospace/blinko:latest → sha256:<digest> (linux/arm64, podman-rootless-v1)
[oci] Lock written: ato.oci.lock.json
[oci] Pulling postgres:14 ...
[oci] Pulling blinkospace/blinko:latest ...
[oci] Starting service: blinko-postgres ...
[oci] Starting service: blinko-website ...
[oci] blinko-website: http://localhost:<host-port>
```

A file `ato.oci.lock.json` is created in the current directory:

```json
{
  "schema_version": "1",
  "oci": {
    "images": {
      "blinko-postgres": {
        "declared_ref": "postgres:14",
        "resolved_digest": "sha256:...",
        "platform": "linux/arm64",
        "provider_semantics": "podman-rootless-v1"
      },
      "blinko-website": {
        "declared_ref": "blinkospace/blinko:latest",
        "resolved_digest": "sha256:...",
        "platform": "linux/arm64",
        "provider_semantics": "podman-rootless-v1"
      }
    },
    "import": {
      "kind": "docker-run-script",
      "source_path": "install.sh",
      "source_hash": "sha256:..."
    }
  }
}
```

## Second run — reuses lock (no re-resolution)

```sh
ato run /tmp/blinko-test --oci-install-sh
```

Expected diagnostics:

```
[oci-install-sh] Selected script: /tmp/blinko-test/install.sh
[oci-install-sh] Extracted services: blinko-postgres, blinko-website
[oci] Checking lock... source hash matches, all entries fresh
[oci] blinko-postgres: reusing sha256:<digest>
[oci] blinko-website: reusing sha256:<digest>
[oci] Lock unchanged (no rewrite needed)
[oci] Pulling postgres@sha256:<digest> ...
...
```

The second run uses the pinned digest from `ato.oci.lock.json`.  Execution
identity is identical to the first run (same digest, same platform, same
provider semantics, same compose source hash).

## Source hash drift — triggers re-resolution

Edit `install.sh` to change the Postgres image tag:

```sh
sed -i '' 's/postgres:14/postgres:15/' install.sh
ato run /tmp/blinko-test --oci-install-sh
```

Expected:

```
Error [E-OCI-LOCK-DRIFT]: install.sh source hash has changed
  lock source_hash: sha256:abc...
  current source_hash: sha256:def...
  Resolve: run `ato run . --oci-install-sh` again to refresh the lock.
```

Ato refuses to run with a stale lock.  The second invocation re-resolves and
updates the lock.

## Cleanup

Stop all containers started in the session:

```sh
ato stop   # or Ctrl-C from the foreground session
```

Containers, the session-scoped network, and session-scoped volumes are
removed.  **Named volumes** (`pgdata`) are preserved (persistent durability).

To also remove persistent volumes:

```sh
podman volume rm pgdata
```

## Known limitations

- Scripts with `if/else/read` prompts: static `docker run` lines are
  extracted; conditional branches are not evaluated.
- `--env-file <path>` flag is not yet supported.
- `--privileged`, `--network host`, and absolute bind mounts are rejected
  with a typed error; the user must adapt the script before running through Ato.
- `docker build` and `docker compose` invocations within the script are
  silently skipped.
