# Component Contract observations

Status: Draft. Extends the OCI service group implementation; not a new Capsule
profile, Contract schema or execution identity.

## Authoring and identity

`[[contract.observation]]` may target any declared logical Port of an OCI
service group. The existing `ato.contract.http@1` GET verifier observes HTTP
responses over that Port's transport. Internal Ports retain their existing
`ato.tcp@1` transport and remain internal: adding an observation does not publish
a service. Single-container/process routes continue to require their Surface
Port. A Port must exist in every declared D for that K; selection cannot rewrite
K or substitute an observation from another Port.

Existing authoring without additional observations produces identical canonical
K and D bytes. No placement, provider, host, container address, readiness result
or observation result enters identity. Component health, cross-component effects
and end-to-end operation results are expressed as distinct GET observations,
including response digests when the result matters. A TCP listener is only a
startup prerequisite. It cannot satisfy an HTTP observation.

## Execution

The CLI resolves internal observation targets from the handles of the containers
it just started. No additional host port is allocated. The native Linux Runner
uses the same owned container addresses. Containers remain on the existing
internal network and all existing stop/recovery rules apply.

Hosted verification passes through the existing fenced validator job and
per-Runner HMAC execution key. An `observation` proof binds Run, Lease, Runner,
logical Port, GET method, exact origin-relative path and expiry. A normal `http`
app-proxy proof cannot select an internal target. The API mints observation
proofs only for the original Rust-validated K projection and the selected
service group's internal Ports. Missing signing authority fails closed.

The Runner reconstructs a single bodyless request with Connection: close. It
forwards no client headers, cookies, bearer tokens, HMAC proof, upgrade or
pipelined bytes to the internal service. Targets come exclusively from live
owned container handles; neither the caller nor K supplies a host URL.

New Runners advertise `runtime_feature=component_contract_observation_v1`.
Hosted admission requires it when K names an internal Port, so an older Runner
cannot treat the observation as an ordinary app request. Existing single-Surface
reports omit the optional HTTP projection, preserving the old report shape.

## Verification

All bytes still pass to the existing Rust runtime verifier and receipt builder.
Every K observation must be Satisfied for Run acceptance; missing, wrong-Port,
wrong-status or wrong-body evidence fails the whole K. No readiness result,
partial component success or HTTP 200 from the public app can replace it.

Unit/integration tests cover authoring, unknown Ports, partial/wrong-Port failure,
scoped authentication and real socket forwarding. Actual OCI and shared staging
results must be recorded separately in the operation evidence; mocks do not
complete that acceptance.
