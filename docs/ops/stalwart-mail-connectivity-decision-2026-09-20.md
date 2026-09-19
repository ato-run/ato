# Stalwart mail connectivity decision — 2026-09-20

This note closes the research gate between the limited network-controls
acceptance and the Stalwart + Missive application fixture. It is deliberately
limited to a private staging fixture. It does not approve public SMTP, direct
MX delivery, production deployment, Discover, or Public Try sending.

## Selected upstream artifacts

### Stalwart

The fixture pins Stalwart `v0.16.22` (source commit
`474dd0229cb20cf513036619781ed97bd8073c3f`) and the immutable Linux amd64
manifest:

```text
docker.io/stalwartlabs/stalwart@sha256:d898e81b67b9f0f989b2aaec05f4c36fe9faa18fd570f8303ccef05c6127cdfe
```

The enclosing multi-platform OCI index is
`sha256:388dcb75a70727c5b551249a6d34b1f1321294852489e4fa3a4e6be698b7c4f0`.
The amd64 image config reports the selected source commit, runs as UID 2000,
and declares `/etc/stalwart` and `/var/lib/stalwart`. For this fixture the
immutable bootstrap config and test certificates come from the read-only
workspace while all mutable registry, configuration and mail data lives under
the one Stalwart state attachment. The fixture must demonstrate this layout;
it must not assume that only message blobs are durable.

Sources: [v0.16.22 release], [tagged source], [Docker installation], and
[v0.16 storage-layout change]. Stalwart identifies its licensing as
`AGPL-3.0-only OR Stalwart Enterprise License v2`; this note records upstream
terms and makes no legal conclusion.

### Missive name resolution

The commercial product at `missiveapp.com` is a hosted SaaS and is not the
artifact selected here. It has no supported self-hosted server image and its
documentation says imported mail is stored in Missive-managed infrastructure.
That product cannot satisfy the previously approved same-Instance design.

The earlier design, however, was specific enough to recover a different,
same-named upstream: [Govcraft/missive]. It is a self-hosted Rust/HTMX webmail
server, connects server-side to a configured JMAP endpoint, supports in-memory
sessions, has no intermediate mail database, and sends with JMAP `Email/set`
and `EmailSubmission/set`. Those properties exactly match the approved
two-application topology and are not properties of the commercial SaaS.

Govcraft/missive currently has no release, git tag, or published upstream OCI
artifact. The fixture therefore pins source commit
`7716106b6c4638426169dac264657437914bbe7f` (crate version `0.5.1`) and builds a
controlled image from its checked-in Dockerfile and lockfiles. The resulting
Linux amd64 manifest digest must be committed to the fixture before it is
admitted to staging. Runtime source builds and floating tags remain forbidden.

There is an upstream licensing metadata mismatch that must stay visible:
`README.md` says AGPL-3.0/commercial while the latest commits add MIT and
Apache-2.0 license files and GitHub detects MIT/Apache. The fixture does not
resolve that mismatch or represent a legal conclusion. Any publication beyond
this closed acceptance requires an upstream clarification/license review.

## Outbound connection decision

Stalwart `v0.16.22` supports an `MtaRoute` relay with a fixed address and port,
SMTP or LMTP, implicit TLS, certificate verification, and optional SMTP AUTH.
A numeric relay address avoids DNS. Direct MX delivery is a different route
and is not part of this fixture.

Stalwart does **not** consume the current `ato.tcp-egress@1` Binding directly.
The selected source resolves a `SocketAddr` and opens `TcpStream::connect` (or
an explicitly bound `TcpSocket`) for SMTP delivery. Its documented proxy
environment does not define SOCKS or HTTP CONNECT behavior. Supplying the
Binding's SOCKS URL to the container without a consumer would therefore be a
false integration.

The fixture adds one explicit, capsule-owned generic transport Adapter:

```text
Stalwart -- internal fixed relay --> transport Adapter
transport Adapter -- SOCKS5 CONNECT --> Ato broker
transport Adapter -- verified TLS/SNI --> controlled fixed SMTP sink
```

The Adapter is byte-oriented. It has one compiled/configured destination,
does not parse SMTP, cannot select a recipient or MX route, and cannot request
an address outside the Instance grant. It performs SOCKS5 CONNECT and the
externally authenticated TLS leg because Stalwart would otherwise verify the
remote certificate against the Adapter's internal service name. Stalwart owns
SMTP AUTH, envelope/message handling and queue policy. Ato remains
payload-opaque and contains no SMTP routing decision.

The controlled sink uses one fixed public IPv4 address and one high port. Its
test CA is explicitly trusted by the Adapter; certificate verification is not
disabled. The Stalwart route targets only the protected internal Adapter and
has STARTTLS disabled on that isolated hop. Acceptance must prove that:

- the named relay route exists and is selected (a missing route may fall back
  to MX, so queue inspection is mandatory);
- the Ato grant contains only the sink IP and port;
- an unapproved SOCKS destination is denied;
- the sink requires authentication and records the accepted message;
- no DNS or general port-25 egress is granted.

Sources: [outbound routing], [outbound strategies], [v0.16.22 relay parser],
[v0.16.22 address resolution], and [v0.16.22 TCP connection path].

## Inbound, TLS and persistence boundary

The private staging proof uses high-port IMAPS and implicit-TLS authenticated
submission. Stalwart supports arbitrary listener ports, but the high-port
proof does not establish ordinary public discovery or standard-port ownership.
The test client must trust the fixture CA and validate the expected SAN; no
`allowInvalidCerts` or equivalent bypass is accepted.

The current fixed TCP forwarder opens a fresh backend connection and does not
preserve the original source address. Stalwart supports Proxy Protocol v1/v2,
but enabling it without an emitting and authenticated proxy would be wrong.
Consequently the staging fixture does not claim source-IP fidelity. Public
exposure remains blocked on end-to-end Proxy Protocol, listener-local exact
trusted-proxy ranges, spoof rejection, and log/policy evidence for the real
client address.

Port 25, 465, 587 and 993 ownership, public MX/A/AAAA/SPF/DKIM/DMARC/MTA-STS,
public certificates, reverse DNS, HTTP management-path isolation, and direct
MX egress are independent production gates.

Sources: [listeners], [TLS certificates], [Proxy Protocol], [DNS setup], and
[security guidance].

## Fixture topology admitted by this decision

The selected Derivation contains three OCI services on one Runner:

1. `stalwart`: owns the only durable state, internal JMAP/HTTP, IMAPS and
   authenticated submission Ports;
2. `missive`: the only Web Surface, using private JMAP and in-memory sessions;
3. `relay`: the explicit generic SOCKS5 + verified-TLS Adapter described above.

The third service is a necessary, recorded consequence of the B investigation;
it is not a third application and carries no mail semantics. It is the only
service receiving `ato.tcp-egress@1`. No service receives unrestricted egress,
host networking, the Docker socket, privileged mode, or an arbitrary host
mount.

The C acceptance must still measure checkpoint size and preserve the existing
limits: checkpoints over 64 MiB, hard volume quota, host reboot, and dockerd
stop are not solved by this topology.

[v0.16.22 release]: https://github.com/stalwartlabs/stalwart/releases/tag/v0.16.22
[tagged source]: https://github.com/stalwartlabs/stalwart/tree/v0.16.22
[Docker installation]: https://stalw.art/docs/install/platform/docker/
[v0.16 storage-layout change]: https://github.com/stalwartlabs/stalwart/blob/v0.16.22/UPGRADING/v0_16.md
[Govcraft/missive]: https://github.com/Govcraft/missive/tree/7716106b6c4638426169dac264657437914bbe7f
[outbound routing]: https://stalw.art/docs/mta/outbound/routing/
[outbound strategies]: https://stalw.art/docs/mta/outbound/strategy/
[v0.16.22 relay parser]: https://github.com/stalwartlabs/stalwart/blob/v0.16.22/crates/common/src/config/smtp/queue.rs#L353-L405
[v0.16.22 address resolution]: https://github.com/stalwartlabs/stalwart/blob/v0.16.22/crates/smtp/src/outbound/lookup.rs#L137-L181
[v0.16.22 TCP connection path]: https://github.com/stalwartlabs/stalwart/blob/v0.16.22/crates/smtp/src/outbound/client.rs#L547-L587
[listeners]: https://stalw.art/docs/server/listener/
[TLS certificates]: https://stalw.art/docs/server/tls/certificates/
[Proxy Protocol]: https://stalw.art/docs/server/reverse-proxy/proxy-protocol/
[DNS setup]: https://stalw.art/docs/install/dns/
[security guidance]: https://stalw.art/docs/install/security/
