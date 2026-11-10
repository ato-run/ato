# Capsulehub coordinator — operator guide

Status: Current (implemented by `services/capsulehub`; not a deployment claim
for any specific environment).
Owner: ato CLI / Objects.
Specification: the draft RFCs in [PR #1718](https://github.com/ato-run/ato/pull/1718)
(`docs/rfcs/draft/CAPSULEHUB_E2EE_COORDINATOR.md` once that PR merges).

`ato-capsulehub` is a self-hosted **blind** distribution coordinator for
already-formed `.capsule` bundles. A client encrypts a bundle locally and
uploads only ciphertext; the shared link carries the object id (SHA-256 of the
ciphertext), the encryption key in the URL fragment, and the claimed
`ContractRef` (`cid`). The coordinator stores and returns opaque bytes.

**The official `ato.run/capsulehub` deployment is this same binary.** There is
no separate hosted implementation and no server-side variant with content
inspection. Any operator can run an equivalent coordinator.

## Trust model (what the coordinator can and cannot do)

The coordinator is **not trusted for confidentiality or integrity**:

| Property | Mechanism |
|---|---|
| Confidentiality | The client encrypts before upload. The coordinator only ever receives ciphertext. The key lives in the link fragment, which HTTP clients do not send. |
| Integrity | The object id is the SHA-256 of the uploaded ciphertext. `ato hub pull` recomputes it from the served bytes and fails closed on any change or truncation. |
| Identity | The link claims a `ContractRef`; the client recomputes it from canonical `ato.contract/1` bytes after decryption and refuses to continue when it disagrees. |

Consequences for the operator:

- The service never sees plaintext, keys, Capsule identity, or bundle
  structure. `PUT` accepts any bytes whose digest matches the path id; `GET`
  returns them unchanged.
- There is no account, no registry row, no listing, no discovery, no ranking,
  and no metadata beyond the object id, byte size, and file mtime.
- **Formed-capsule admission is a client-side rule.** The coordinator cannot
  enforce it, because it cannot read the bytes. `ato hub push` validates before
  encrypting and `ato hub pull` validates again after decrypting. Do not add
  content validation here: a coordinator that inspects bytes is outside this
  design and breaks the trust model.

## Quick start

```sh
cargo build -p capsulehub --release
./target/release/ato-capsulehub \
  --bind 127.0.0.1:8787 \
  --storage /var/lib/ato-capsulehub \
  --put-token "$(head -c 32 /dev/urandom | base64 | tr -d '\n')"
```

Terminate TLS in front of the coordinator (or bind a private interface and use
a tunnel). The coordinator deliberately has **no default bind address**; an
operator must choose one explicitly. Binding a wildcard address logs a warning.

## Configuration

Every value is a flag with an `ATO_CAPSULEHUB_*` environment alias. Tokens are
redacted from logs and `Debug` output.

| Flag | Environment | Default | Notes |
|---|---|---|---|
| `--bind` | `ATO_CAPSULEHUB_BIND` | **required** | Socket address, e.g. `127.0.0.1:8787`, `[::]:8787`. No implicit public bind. |
| `--storage` | `ATO_CAPSULEHUB_STORAGE` | **required** | Storage root; `objects/` and `tmp/` are created inside. |
| `--max-object-bytes` | `ATO_CAPSULEHUB_MAX_OBJECT_BYTES` | `1073741824` (1 GiB) | Maximum ciphertext bytes per object. Uploads beyond it are rejected with 413 before anything is stored. |
| `--max-storage-bytes` | `ATO_CAPSULEHUB_MAX_STORAGE_BYTES` | unset (unlimited) | Total storage budget. Uploads that would exceed it are rejected with 507. |
| `--put-auth` | `ATO_CAPSULEHUB_PUT_AUTH` | `token` | `token` or `open`. Use `open` only on a trusted network. |
| `--put-token` | `ATO_CAPSULEHUB_PUT_TOKEN` | **required in `token` mode** | Bearer token for `PUT`. No default. |
| `--delete-token` | `ATO_CAPSULEHUB_DELETE_TOKEN` | unset | Operator token for `DELETE`. When unset, `DELETE` always returns 403. |
| `--rate-limit-per-minute` | `ATO_CAPSULEHUB_RATE_LIMIT_PER_MINUTE` | `60` | Per client IP, fixed one-minute window. `0` disables it. `/healthz` is never rate limited. |
| `--retention-seconds` | `ATO_CAPSULEHUB_RETENTION_SECONDS` | unset (persistent) | Optional TTL sweep; abandoned partial uploads also expire. |
| `--allow-anonymous-read` | `ATO_CAPSULEHUB_ALLOW_ANONYMOUS_READ` | `true` | When `false`, `GET`/`HEAD` require the upload token (requires `--put-auth token`). |

Startup fails with a non-zero exit code and a single-line error for invalid
configuration (missing bind/storage, missing token in token mode, zero sizes,
`--allow-anonymous-read=false` with `--put-auth open`).

## HTTP surface

```text
PUT    /v1/capsulehub/objects/{sha256-hex}   upload ciphertext (401 without the upload token in token mode)
GET    /v1/capsulehub/objects/{sha256-hex}   download ciphertext (200 with Content-Length, Content-Type: application/octet-stream, ETag: "<id>")
HEAD   /v1/capsulehub/objects/{sha256-hex}   headers only, same as GET
DELETE /v1/capsulehub/objects/{sha256-hex}   operator takedown (403 without the delete token)
GET    /healthz                              liveness ("ok", no configuration details)
```

Status codes:

| Case | Status |
|---|---|
| New object stored | `201` |
| Idempotent re-PUT of the same id | `200` (same bytes; nothing changes) |
| Body digest differs from the path id | `400`, nothing stored |
| Object id is not `sha256:<64 lowercase hex>` | `400` |
| Body over `--max-object-bytes` | `413`, nothing stored |
| `--max-storage-bytes` would be exceeded | `507`, nothing stored |
| Missing/wrong upload token (token mode) | `401` |
| Missing/wrong delete token (or no delete token configured) | `403` |
| Unknown object | `404` |
| Rate limit exceeded | `429` with `Retry-After: 60` |
| Wrong method on a known route | `405` with `Allow` |

## Storage layout

```text
<storage>/
  objects/<64 lowercase hex>   complete objects, named by their SHA-256
  tmp/<pid>-<nanos>-<n>.partial  in-flight uploads
```

An upload streams into `tmp/`, is hashed while writing, is `fsync`ed, and is
compared against the path id. Only then is it atomically renamed into
`objects/`. Any abort, digest mismatch, size rejection, or quota rejection
removes the partial file. Restarting the process keeps stored objects and
recomputes the storage usage by scanning `objects/`.

Two concurrent `PUT`s of one id both succeed (or one wins) with identical
bytes; an interrupted upload leaves nothing behind.

## Operator duties

### Takedown is by object id only

A blind coordinator cannot classify content, so it cannot decide that an
object violates a policy by looking at it. Takedown is:

```sh
curl -X DELETE -H "Authorization: Bearer $ATO_CAPSULEHUB_DELETE_TOKEN" \
  https://hub.example/v1/capsulehub/objects/sha256:<64 hex>
```

Reported links name the object id (`#o=...`). Removal does not and cannot
identify who uploaded the object, when, or what it contains. Keep the delete
token out of client machines and out of the upload path; it is an operator
credential, not a client credential. Rotating it is a restart with a new value.

### Retention

By default objects are persistent. With `--retention-seconds=N`, a background
sweep removes objects older than `N` seconds and abandoned partial uploads
older than `N` seconds. Pick `N` together with your quota and abuse policy, and
publish it where links point.

### Quota and limits

Choose `--max-object-bytes`, `--max-storage-bytes`, and
`--rate-limit-per-minute` from your capacity and abuse tolerance. The native
build sizes in the portable-native RFC set the practical object-size floor for
real bundles; the client caps plaintext at 768 MiB, plus ciphertext framing
overhead.

### Blinded-storage liability

Operating this service means relaying opaque bytes you cannot inspect:

- you cannot enforce law, age policy, license terms, or category over the
  content you relay;
- you cannot prove what an object contains, only that it was addressed by a
  digest;
- you must publish the operations policy where your links point: removal
  contact, retention, limits, jurisdiction;
- this liability posture is why capsulehub is deliberately separate from the
  Store and from ato.run's normal content surfaces.

If a jurisdiction requires content-level moderation, this service design is
not the right mechanism for that deployment; do not add server-side
inspection to it.

### Reverse proxies and client IPs

Rate limiting keys on the accepted TCP peer address. No forwarding header is
trusted, so behind a reverse proxy every client shares the proxy's address.
Either keep the limit generous, disable it (`0`), or deploy the coordinator
directly on the address clients reach. Health checks are exempt.

### Monitoring

- `GET /healthz` — liveness; returns `ok` and nothing about configuration.
- Storage usage is exposed only via the process's own accounting; use your
  filesystem monitoring for `objects/` size and inode counts.
- `RUST_LOG=capsulehub=debug` enables per-request diagnostics (object ids and
  byte counts only — never headers or bodies).

## Security notes

- The coordinator never receives keys and never sees plaintext. A hole in the
  client's link handling cannot be fixed here.
- Upload and delete tokens are compared in constant time over fixed-length
  digests; neither value nor its length is exposed by comparison timing.
- Tokens are never logged and never appear in `Debug`/`Display` output.
- Error bodies are static strings and never echo request headers or
  credentials.
- Bind explicitly and use TLS termination. `ato hub push` and `ato hub pull`
  refuse plain HTTP except loopback, and `ato hub pull` never fetches or
  executes hub-served HTML.

## Non-goals

No discovery, search, listing, categories, accounts, author identity, review,
mirroring/federation, or content inspection. No server-side Contract
verification. The Store pipeline and the hosted-import quarantine path are
unchanged and separate.
