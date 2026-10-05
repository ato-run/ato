# Formation Session external Producer relay

Status: draft implementation contract. This prepares an isolation boundary;
Native Codex/Claude Code tool inventory, authentication and actual exploration
acceptance remain separate gates. It grants no Search or Runtime authorization.

## Ownership and modes

`ato-formation-session-mcp --connection PATH` keeps the existing direct stdio
mode. Only an owner process outside the Native sandbox may read the private authorization component beside this public Session
connection. The strict `ato.formation-session-connection/2` JSON contains only
schema, address, search_id, configuration_ref and agent. Its sibling `.capability`
is a non-serializable binary component bound to that immutable scope; address may
change on reconnection. It is not an owner credential or Runtime ticket. Public
status/export/diagnostics never serialize this component. Descriptor inspection
alone cannot authenticate. Existing v1 records are preserved; this server refuses
v1 rather than silently importing a token-bearing descriptor.

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
bounded MCP stdio handler. The model tool inventory is `status`, `next`, `submit`.
`cancel` is rejected by the MCP command dispatcher and remains owner CLI only.
An Agent uses a typed decline when it cannot proceed.
Source/path/URL/Search/credential parameters are not added. Existing
typed `output_json`, input digest, exchange binding, duplicate rejection, saved
response idempotency, ACK/UNKNOWN reconciliation and cancellation cleanup remain
owned by the original Session/Coordinator/Rust authority.

## Producer descriptor

The strict `ato.formation-session-relay/2` public descriptor contains only schema,
loopback address and immutable binding (Search, configuration, agent, expiry).
The Unix v2 descriptor uses a socket instead of address. Neither descriptor
contains a token, private component path, owner connection path, Ato credential,
private input, Runtime ticket, grant, Source path or successful Derivation.

`--publish-capability-file PRIVATE_PATH` selects the private authorization output;
`--capability-file PRIVATE_PATH` selects it for the fixed stdio relay process.
The owner launcher places it outside the public package. If omitted, the owner
CLI uses the descriptor's `.capability` sibling. This default is publication
hygiene, **not** a model isolation boundary. Native/model tools may not read that
file or discover its path through MCP tools. Only the authentication host/fixed
MCP transport may read it; Codex Code Mode retains a separate deny-default OS
profile, and Claude exposes no built-in file/process tools.

The private relay component is a bounded binary file: SHA-256 of the serialized
public descriptor followed by a random 64-character lowercase-hex key. It is
loaded into a non-Debug transport object and skipped during serialization.
Copying it to another descriptor cannot authorize that scope. Missing, malformed,
symlinked or mismatched authorization fails closed before forwarding. Private
components use mode-0600 atomic no-clobber publication, file/directory sync and
regular-file/inode checks. These checks do not prove isolation against the same
UID: actual OS negative reads are required. This implementation does not claim
memory-only authorization or FD delivery. Startup/status errors are redacted;
owner inspection commands return the public view, never raw authorization.

Public and private publication never overwrites existing files. If an interrupted
publication leaves only a private component, the owner retains it and explicitly
chooses a fresh publication path after reconciliation; the launcher does not
replace or print it. Old descriptors and measurement evidence remain unchanged.

Publication fails if the destination already exists, including a symlink. A
client reads a bounded regular descriptor once, rejecting symlinks, unknown
fields, external addresses, private capability fields and oversized data. On Unix it
compares device/inode before reading to reject replacement races. It never reloads
another descriptor or connects to a replacement address. Owner reconnection
requires explicit fresh publication after saved-status reconciliation; stale
descriptors are retained as evidence. Broker shutdown does not cancel a Search.

## Transport

### Unix socket

On Unix, an owner may select `--relay-socket /absolute/path/to/fresh.sock`
alongside `--connection` and `--publish-relay`. The same `--relay` client
recognizes the strict `ato.formation-session-unix-relay/2` descriptor, which
replaces `address` with `socket` and retains the same public binding. Authorization remains separate.
Authenticated request/reply wire framing stays unchanged. This adds a physical
transport to the existing three model tools, not another CandidateProducer engine.

The socket path must be absolute, canonical through its parent, free of control
characters and at most 100 UTF-8 bytes. The owner never unlinks a conflicting
file, socket or symlink. Failed publication removes only the socket just created;
shutdown removes it only when its saved device/inode still matches. The expired
descriptor remains as evidence. Cleanup never cancels or reopens a Search.

Both transports call the same authenticated frame handler. Unix connections use
a bounded connect, nonblocking I/O and poll against the absolute frame deadline.
Darwin rejects timeout updates after a peer closes even when unread response
bytes remain; poll lets the same saved response be read and reconciled after a
disconnect without another inference, execution or budget reservation.

An OS profile can allow only the literal Unix socket, while denying owner files,
other sockets and TCP. Mode 0600 remains publication hygiene, not isolation.
Native authentication, provider egress, Skill discovery and real same-K receipt
acceptance still require separate evidence with the selected product versions.

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
