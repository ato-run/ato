# ADR-029 — Coordinator receipt acceptance through the common Rust authority

Status: implemented locally; not deployed. Follows ADR-026, ADR-027 and ADR-028.

A Runtime's `pass` label is not verification evidence. The Coordinator must
accept the full effective K before recording a VerifiedRoute, including when
recovering a crash between the attempt CAS and route insertion. Requester
acceptance remains independent and mandatory.

## Frozen expectation

The current internal Runtime Network v0 request now carries `base_contract`.
It is the same canonical BoundContract the requester froze before dispatch.
Rust checks its schema, unique requirement ids and canonical digest against
`base_contract_ref`; Browser v0 normalization and the effective ref are checked
as well. The Coordinator persists these request bytes before issuing tickets.
The receipt's own claims never supply the expected K.

When a finished PASS arrives, authenticated Runtime/fence and attestation checks
still belong to TypeScript. `ato-formation::receipt::accept_verified_route`
checks the frozen K, authorized D, exact attempt, Runtime/environment placement,
satisfied base receipt and complete Browser receipt. The WASM adapter requires
an attempt id, and has no legacy placement-only lookup. TypeScript does not
implement K evaluation or Browser verdict aggregation.

## WASM boundary

`tools/receipt-authority` builds with `ato-formation` default features disabled.
`planning` remains the default for native consumers; archive decompression,
source detection, build planning and runtime execution are not in the module.
`tools/receipt-authority/build.sh` uses the pinned toolchain and locked packages.

ABI 1 exposes version, bounded input allocation, and JSON evaluation. A fresh
instance owns its buffers for each call; Rust owns them through instance
retirement. There is no manual raw-pointer dereference or shared caller state.
Input is limited to 1 MiB and linear memory to 64 MiB. The host checks the ABI,
output bounds and decision shape. A load failure prevents the Worker starting;
ABI mismatch, trap, malformed input or invalid receipt never becomes PASS.

The API vendors the module with its hash and source revision. Rebuilding is an
explicit local/release step, not a runtime download. The module has no imports,
network access, clock, process launcher or secret bindings.

## Settlement and UNKNOWN

Unknown attestation states are handled before receipt verification. A late
result for an UNKNOWN attempt remains resolution evidence only, even if valid.
A known finished result whose receipt is rejected is recorded inconclusive
with the authority's reason; existing effect-based fallback policy applies only
to this known result. CAS/DB UNKNOWN barriers from ADR-028 remain authoritative.

A saved pass with no route is revalidated against the saved request on recovery.
Only a valid saved receipt can insert the route, idempotently by attempt id.
Historical v0 requests without frozen K cannot be reconstructed from their
receipts and cannot gain new routes. Internal v0 is updated together in both
repos; this is not a backward-compatible remote rollout or a deployment.

## Evidence and limits

Native Rust and Worker WASM share golden base/Browser fixtures. Tests cover
forged PASS, failed base receipt, missing Browser, FAIL/INCONCLUSIVE Browser,
wrong attempt/environment/effective K, ABI failure and traps. Coordinator
integration retains UNKNOWN races, late results and settlement crash injection.
Browser PASS remains model-judged evidence, not deterministic proof of behavior.
Measurements distinguish actual module size/linear memory, local Worker HTTP
latency and Node CPU reference cost; local measurements are not Cloudflare
production billing CPU or a deployed load test.
