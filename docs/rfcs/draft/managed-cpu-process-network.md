# Managed CPU process network

Status: Draft, implementation for the six-Runner production CPU pool.

A namespace process uses the existing scoped network transport. The namespace
has no host network interface. Trusted host ingress bridges reach only the
declared ports of that Run. Undeclared runtime egress is denied; granting new
egress is a separate Adapter change, never a host-network fallback.

The process owns its bridges until confirmed teardown. TCP readiness uses the
namespace-side application handshake, not the host bridge's listening socket.
HTTP readiness still observes the application response. State is packed only
after the existing process-group stop confirmation.

The namespace implementation advertises `isolation:runtime_launch=untrusted-v1`.
Host-boundary execution retains its existing implementation and advertises the
distinct scoped host-boundary capability. Capsule/Contract identity, selected
Derivation, admission conditions and verification observations are unchanged.

Acceptance is a real Linux namespace launch with HTTP and SQLite, a denied
host-loopback canary, absence of the Runner credential in the workload, closed
ingress after stop, and the existing two-Run state continuation regression.
