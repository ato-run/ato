# Producer network profile for Formation Session

Status: draft, prepared macOS profile only. Existing Capsule sandbox policies,
Session protocol, authorization, budgets and Runtime authority stay unchanged.
This implements a physical endpoint ACL; it is not Native agent acceptance.

The Owner may optionally pass `--relay-endpoint 127.0.0.1:PORT` and at most four
`--provider-endpoint PUBLIC_NUMERIC_IPV4:443` options to the public isolation
preflight `prepare` operation. All addresses/ports must be canonical literals.
Default is no networking. Hostnames, URLs, wildcards, ranges, zero/overflow ports,
IPv6, private/link-local/multicast/reserved provider IPs and duplicate entries
fail closed before creating output. No DNS lookup, routing discovery, automatic
endpoint refresh or fallback is performed. Provider identity is always recorded
as unverified. Owner approval and real provider identity/TLS routing remain gates.

The generated Seatbelt profile allows only exact `remote tcp "IP:PORT"` entries
over its deny-default policy. It adds no blanket network-outbound/inbound,
system-socket, DNS, UDP or Unix-socket rule. Existing public/scratch paths and
fixed executable literals are unchanged. Verification regenerates the profile
from its frozen metadata as well as checking hashes, so rehashing a broader
policy cannot bypass the helper. Provider endpoint metadata contains no auth.

The helper does not consume a relay descriptor or Owner connection/capability.
Its fixed MCP argv is `--relay <public>/session-relay.json`, not `--connection`.
The descriptor remains absent and binding unchecked at preparation. An actual
Owner publication and final package freezing need a separate gate: its exact
address must equal the allowed endpoint and its expiry must not exceed saved
Session deadline, checked through the existing Rust/auth authority.
The immutable Search/configuration/agent/expiry binding and request/reply HMAC
remain the existing Rust relay's authority. A prepared TCP permission alone
does not authorize a Search or prove that its actual descriptor matches.

The C-only OS probe uses the same rule generator with controlled loopback relay
and provider substitute endpoints. Positive connects and wrong-port,
UDP, IPv6, Docker/Owner Unix socket denials are tested against live destinations;
IPv6 wrong-address listeners use the same allowed ports. Secondary IPv4 cannot
be bound on the existing macOS interface without a global alias change; its
additional denial still requires EPERM/EACCES but live reachability is unmeasured.
No interface/alias is modified.
only EPERM/EACCES counts as denial. Private files are same-UID generated canaries,
including a synthetic Owner connection. Extra descriptors are closed in the
launcher and checked absent in the C process; no real credentials or descriptor
capabilities are read/inherited. The original filesystem/process denials also
remain tested. New probe packages declare `os_fixture_protocol = 2`; old records
and packages are not reinterpreted. Output is a fixed boolean inventory, never
canary values, raw captures or parser/subprocess error bodies.

IP/port ACLs cannot validate TLS SNI, certificates or distinct origins sharing a
CDN IP. DNS remains denied; this unit does not invent a Native SDK flag or grant
resolver IPC to make an untested authentication route work. Actual public
provider reachability, Native DNS/TLS/auth transport, tool inventory, restricted
MCP connection and private-data negative tests need an independently approved
fixed plan. If that route cannot operate under these permissions, remain blocked
with its measured reason rather than adding general networking. No Native login,
auth, inference, Search, Runtime Run or paid API invocation is part of this unit.
