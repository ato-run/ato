# Formation Session external Producer relay

Status: draft implementation contract. This prepares an isolation boundary;
Native Codex/Claude Code tool inventory, authentication and actual exploration
acceptance remain separate gates. It grants no Search or Runtime authorization.

## Ownership and modes

`ato-formation-session-mcp --connection PATH` keeps the existing direct stdio
mode. Only an owner process outside the Native sandbox may read this private
Session connection, which contains its existing Session capability.

`ato-formation-session-mcp --connection PATH --publish-relay NEW_PATH`
serves an external loopback broker for that already scoped Session. The optional
`--relay-expiry-ms UINT64` selects a shorter Unix-millisecond expiry. Publication
requires a saved status with the same Search/configuration and an unexpired
deadline. Expiry cannot exceed that original deadline. Publication never creates
a Search, reasoning exchange, reservation, attempt, grant, or a new deadline.

`ato-formation-session-mcp --relay PATH` serves the same stdio MCP interface
through the immutable Producer descriptor. It never opens the owner connection.
These modes are mutually exclusive; publication requires `--connection`.

The broker and relay call the existing `FormationSessionMcpServer.handle` and
bounded MCP stdio handler. The tool inventory remains `status`, `next`, `submit`,
`cancel`. Source/path/URL/Search/credential parameters are not added. Existing
typed `output_json`, input digest, exchange binding, duplicate rejection, saved
response idempotency, ACK/UNKNOWN reconciliation and cancellation cleanup remain
owned by the original Session/Coordinator/Rust authority.

## Producer descriptor

The strict `ato.formation-session-relay/1` descriptor has only:

- `schema`, a loopback `address` with a nonzero port, and `capability` containing
  32 random bytes encoded as 64 lowercase hex characters;
- `binding.search_id`, `binding.configuration_ref`, optional agent metadata, and
  immutable `binding.expires_at_ms`.

It contains no Ato auth, owner connection path/capability, private input value,
Runtime ticket, grant, Source path, DB path or successful Derivation. It is a
limited Producer capability and must not be printed or included in public logs.
On Unix publication uses a mode-0600 temporary file in the destination directory,
file sync, atomic `persist_noclobber`, and directory sync. This permission is not
a secrecy claim against a model running as the same UID: the whole-process OS
boundary must expose only the Producer descriptor and deny the owner file.

Publication fails if the destination already exists, including a symlink. A
client reads a bounded regular descriptor once, rejecting symlinks, unknown
fields, external addresses, invalid capabilities and oversized data. On Unix it
compares device/inode before reading to reject replacement races. It never reloads
another descriptor or connects to a replacement address. Owner reconnection
requires explicit fresh publication after saved-status reconciliation; stale
descriptors are retained as evidence. Broker shutdown does not cancel a Search.

## Transport

The broker binds only `127.0.0.1` on an ephemeral port. Both directions use
bounded newline-delimited JSON, one request/reply per connection. The client
connects only to its exact saved loopback address, without retries or fallback.
The outer request is bounded to 270,336 bytes; the MCP request remains bounded
to 262,144 bytes. The response is bounded to 524,288 bytes. Descriptor size is
bounded to 8,192 bytes. Read/write/connect timeouts are two seconds; the client
reply read timeout is eight seconds to accommodate the existing bounded Session
transport. Read/write deadlines apply to the whole frame, so partial-byte drips
cannot renew them. The broker handles connections serially and rejects new requests
after expiry. An in-flight request remains subject to the Session authority's
deadline and effect/cleanup rules.

The capability itself is never sent over TCP. `ato.formation-session-relay-request/1`
contains a fresh 32-byte random hex nonce, the exact frozen binding, MCP request,
and HMAC-SHA256 proof. Its proof covers the JSON serialization of the tuple
`[request_schema, nonce, binding, request]` using the decoded capability as key.
The response contains `ok`, optional MCP `response`, and a proof covering
`["ato.formation-session-relay-reply/1", nonce, request_proof, ok, response]`.
The `hmac` library performs constant-time verification. Domain separation and
nonce/request binding reject request reflection and unrelated reply replay; a
process that takes the old port cannot manufacture accepted MCP evidence.
Transport replay cannot create a second saved proposal: existing Session
idempotency remains the authority. No new execution or budget ledger is added.

Unauthenticated/invalid outer requests receive only an opaque negative reply.
Client failures return a fixed redacted MCP error directing saved-status
reconciliation. Startup diagnostics exclude paths, parser text, connection
contents and capabilities. Raw untrusted errors are not logged.

## Acceptance separation

Local synthetic Session/MCP tests can establish descriptor publication, scope,
framing, authentication, redaction, disconnect reconciliation and common
idempotency. They cannot establish Native login IPC, model tool inventory,
provider egress, Source/private-file negative reads, Skill discovery, actual
inference, Runtime execution, fresh same-K receipts, or owner-authorized plans.

Prepared macOS Native profiles must separately admit this exact relay endpoint
and fixed MCP binary while keeping owner Session/private files inaccessible.
No Native configuration flag, global login change or credential-file fallback
is implied by this RFC. Expired relays reject Producer operations; the owner
must reconcile ACK, UNKNOWN, cleanup or saved responses through its existing
private authority, including the separate post-deadline reconciliation path.
