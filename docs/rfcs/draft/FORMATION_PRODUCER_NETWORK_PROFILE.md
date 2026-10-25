# Producer network profile for Formation Session

Status: draft, prepared macOS profile only. Existing Capsule sandbox policies,
Session protocol, authorization, budgets and Runtime authority stay unchanged.
Native profiles remain network-denied; this is not Native agent acceptance.

The Owner may pass `--relay-endpoint 127.0.0.1:PORT` as an explicit request to the
public isolation preflight `prepare` operation. Addresses/ports must be canonical literals.
Default is no networking. Hostnames, URLs, wildcards, ranges, zero/overflow ports,
IPv6, private/link-local/multicast/reserved provider IPs and duplicate entries
fail closed before creating output. The valid relay request also fails with
`blocked_scope_wider_than_loopback` before binary reads or publication:
measured `localhost:PORT` permits the same host's non-loopback interfaces as well
as IPv4 and IPv6 loopback. This wider scope does not satisfy the required
loopback-only OS boundary. `--provider-endpoint PUBLIC_NUMERIC_IPV4:443`
validates an explicit request but rejects with
`provider_egress_fixed_ip_unsupported` before binary reads or output publication:
the measured macOS remote TCP grammar cannot express an exact public IPv4 ACL.
No wildcard, unrestricted network grant or provider proxy replaces this gate.
No DNS lookup, automatic endpoint refresh or fallback is performed. Provider
identity remains unverified and provider egress remains blocked/unconfigured.

The controlled C fixture uses supported `remote tcp "localhost:PORT"` over its
deny-default policy. Native prepared profiles have no network rule. The helper
never emits a numeric-IP SBPL as usable. It adds no blanket network-outbound/inbound,
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

The C-only OS probe uses the same rule generator with two controlled loopback
ports. Positive IPv4 and IPv6 connects, wrong-port, UDP, Docker/Owner Unix socket,
and non-loopback denials are tested against live destinations. The non-loopback
listener uses an existing local interface at the same allowed port; an outside
connection confirms its reachability. Fixture-only interface discovery performs
no DNS or external connection and publishes no address/name. No interface/alias
is modified. Only EPERM/EACCES counts as denial. Private files are same-UID generated canaries,
including a synthetic Owner connection. Extra descriptors are closed in the
launcher and checked absent in the C process; no real credentials or descriptor
capabilities are read/inherited. The original filesystem/process denials also
remain tested. New probe packages declare `os_fixture_protocol = 2`; old records
and packages are not reinterpreted. Output is a fixed boolean inventory, never
canary values, raw captures or parser/subprocess error bodies.

Failed measurements 03 (unavailable loopback alias), 04 (numeric-IP grammar
rejected before the C main), and 05 (localhost scope permits live non-loopback
local interface) remain preserved with their original code/package pins. A
bounded diagnostic confirms the 05 connection actually succeeded, rather than
merely receiving a different error. No network boundary is accepted as complete.

This profile cannot grant exact public provider IP egress. A future solution
must also validate TLS SNI, certificates and distinct origins sharing a CDN IP.
DNS remains denied; this unit does not invent a Native SDK flag or grant
resolver IPC to make an untested authentication route work. Actual public
provider reachability, Native DNS/TLS/auth transport, tool inventory, restricted
MCP connection and private-data negative tests need an independently approved
fixed plan. If that route cannot operate under these permissions, remain blocked
with its measured reason rather than adding general networking. No Native login,
auth, inference, Search, Runtime Run or paid API invocation is part of this unit.
