# Fixed managed embedding transport over existing OCI network authority

Status: draft; locally implemented and CPU Docker tested. Not deployed/enabled.

The existing lease `NetworkAuthorization` may carry `managed_embeddings` with
`service_id`, `api_key_binding_id`, `base_url_binding_id`, `broker_origin`. Omitted
legacy authorization is unchanged. API derives the normalized HTTPS origin
only from trusted `STORE_URL`; application/request URL input is never accepted.
The selected service must declare both exact SDK Binding protocols/scalar refs.
The Runner independently checks selected service/grant references/environment
names and redeemed short-token/baseURL pair. Direct TCP egress for that service
is rejected before any network/workload creation. Existing Runner/Lease/Run
identity and sealed Binding authority are reused, without a new auth/table/root.

The issuer requires `network=ato.managed-embedding@1`; old Runners cannot be
selected. The Runner advertises it only with existing OCI/network controls.
A relay owns the existing per-service `create_egress` internal bridge and fixed
1080 listener, using the already enforced default-deny host firewall. Only its
service joins it. Existing ServiceEgress/group guards own cancellation/cleanup.
`OPENAI_BASE_URL` is rebound only when its exact previously redeemed scalar
matches the authorized origin; ordinary network/environment collisions still
fail. No undeclared variable, provider key, host networking or Docker socket is
introduced. The key is the same short per-Run fixed-operation capability.

Relay accepts only `POST /embeddings HTTP/1.1` and its exact capability. It
constructs the only upstream URL: trusted HTTPS origin plus
`/v1/managed-secrets/embeddings`. Request headers/cookies/URLs are not forwarded;
only the existing capability and fixed JSON/identity-encoding headers are set.
No redirects, ambient proxies or encoded payloads are accepted. Bounds: eight
concurrent requests, 8 KiB headers, 32 KiB input, 128 KiB response, five-second
read/request deadline, two-second upstream timeout. Shutdown closes owned input
sockets and listeners; an already admitted upstream HTTP operation is bounded
by its two-second timeout and existing server-side Run/Lease revocation.

The Broker remains the only upstream-credential authority. K/D/verification
semantics are unchanged. Native/process paths do not gain network access.
Staging/hosted full lease dispatch and deployment are separate acceptance gates.
