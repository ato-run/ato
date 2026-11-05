# Portable Instance origin Binding

Status: Draft. Not deployed or verified on a hosted Runner.

Some self-hosted applications need their externally reachable origin to
generate absolute URLs and restrict WebSocket origins. The value is a property
of the physical Instance and its current ingress, not part of K, D, an image,
or the sealed workspace. A service group can have multiple consumers, while
the current group profile deliberately permits each Binding id to reach only
one service. Each consumer therefore declares a separate Binding using the
same explicit protocol, rather than broadcasting a secret or a runtime value
to every service.

The proposed `ato.instance-origin@1` Binding is a non-secret origin allocated
and supplied by the hosted control plane for the actual Instance. A Runner
projects it under the existing `ATO_BINDING_<ID>` environment name, scoped to
the single declared consumer. Local CLI users may supply the value explicitly.
The declared protocol and ids are sealed as usual; their per-Instance values
are not included in K, D, transport identity or a verification receipt.

Hosted import and wake must reject owner-supplied values for this protocol.
They resolve the active Instance's own host slug from an owner-fenced database
row and the configured app-proxy base host. Invalid or unavailable hosts fail
closed, before a lease is made claimable. The origin is an HTTPS origin without
credentials, path, query or fragment. No external network, host ownership,
public accessibility or traffic authorization is granted by this Binding.
Every opted-in service receives the same resolved value for that Run. A copied,
restored or newly created Instance recomputes its own value; it does not inherit
the source Instance's origin.

Import metadata and Discover setup prompts must distinguish this managed value
from a user-entered secret. Unknown Binding protocols remain unsupported by
publication. Static Web has no runtime projection and remains ineligible for a
required origin Binding. Public trial and shared-access behavior must use the
same actual app-proxy origin, never a request-supplied host. The hosted and CLI
Contract verifier remains Rust-owned; injecting an origin is not evidence that
the app started or that K is satisfied.

Required validation covers absent and malformed hosts, ownership mismatch,
caller override, multiple consumer ids, different Instances and restarts,
user-entered secret coexistence, no disclosure in public metadata or receipts,
service-scoped projections, and actual hosted ingress and WebSocket behavior.
This proposal does not grant outbound traffic, address DNS custom domains, or
establish a publication-ready Fleetbase build.
