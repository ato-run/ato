# D3-E v2 execution driver review

This is the current #1431 review. The `/1` review remains historical evidence of
the request-capture blocker; it is not the current driver state.

## Pins and scope

- Preregistration #1432 merged: `d19be91e5b953ebc12f4f744931989cad384fe36`.
- Controller base: the same merge; tested implementation: `bf887f230b53db6f53004b9759b78d1bbce0d261`.
- Controller bundle hash: `46ff144606c311e721e8e4accd0ed60f21848d7abcc2421bf599050236249f1a` (sorted compact JSON file-hash map).
- Plan v2: `a3af8f45f7641f7c367ff8d118b2d4e72abcf05711382ea2a7d3864548bd2249`; v1 and v2 plan bytes unchanged.
- Execution: `d1dde994c2d310a725b546921f9b3455777e28da`. Application/budget binaries built ONLY from this clean pin.
- API: `38668a97e7632256b33074b0d223670c16b76bfd`; WASM: `d4732b46f1ce5ab3d1193d957d7bcb2d24f0469ffaa80e267c6cfda5bccb8a46`.
- Prompt: `a3217b28b4242fdc03e11fe5dee7d91fbbc47b79358c4ae31601002b7a81ea50`.
- No CandidateProducer/compiler/API/Verifier/authority changes. No additional model client.

## Exact request evidence

Preclaim projection checks only source/K drift and G2's actual known-D FAIL.
After the final timeout is set, pinned Rust atomically records RequestEvidence
with its reservation, before credential read/send. `proposal_budget inspect-request`
is the sole controller source for request/body hashes, byte lengths and timeout.
Python never parses the journal or reconstructs a provider body. Snapshot checks
reject unresolved reservations, duplicates, mismatch and restart event changes.

## Verified gates (no live model)

- E0–E19 PASS; 22 driver test methods.
- Original prereg 8 + v2 prereg 7 + Linux credential 7 PASS: **44 unique offline tests, 0 failures**.
- Control helper Clippy/rustfmt and diff check PASS.
- Unchanged core pin: 668/0/1, recorded in `formation-deepseek-d3-v2-verification.json`.
- **G0–G5 actual mock integration PASS**, six loopback synthetic responses.
  Actual isolated local Coordinator/D1, pinned WASM, Linux Runtime and Verifier.
- G1/G2/G5 have actual same-K PASS receipts. G2 first ran the registered known D
  and produced an actual HTTP404 verification failure; the requester projected it.
- G3 unauthorized shell field is rejected; G4 unsupported terminates without a route.
- G5 completion commits, only its Coordinator response is dropped, requester restarts,
  persisted raw is reused, and the same journal snapshot remains unchanged.
- Mock server exact body bytes match every journal body hash. Request JCS hashes
  match message content bytes, with actual post-claim timeouts recorded.
- Source PUT forwarding and actual C1 deadline wire-field defects found during
  the mock run were corrected in the controller, not in the pinned code.

This is **not** D3 live evidence and does not close the 5b live gate.
See the paired JSON for receipts, attempts, generated D refs, raw synthetic output,
request evidence, G2 known failure and reproducible hashes.

## Limited credential injection

The user authorized a separate post-preflight injector. It runs A–M first, then
requests stdin only after PASS, keeps the value in sealed anonymous memory, and
sends the fd directly to a requester-only wrapper over a private owner-only Unix
socket. The controller receives only the socket pathname: **no secret bytes, fd,
environment value or stdin**. Runtime/Coordinator/helper environments are explicit
non-secret allowlists and inherit no credential descriptors.

The wrapper injects only `DEEPSEEK_API_KEY` into the pinned Rust Requester.
Its reservation-before-env-read/send ordering is unchanged. All confinement tests
use a public synthetic canary; the real `.dev.vars` has not been read or checked.
No key value/hash/header/artifact/argv is emitted. The injector is not a model client.

## Pricing and budget

Fresh official pricing fetch during this mock preflight:
`2026-09-28T23:56:46.661435+00:00`. Model `deepseek-flash`, V4.1-Flash,
peak cache-miss input $0.30/M and output $1.20/M. The exact page hash and URL are
in the JSON. **Live execution must fetch again** within one hour, rejecting increases.

Six calls maximum, input cap 262144, output cap 2048, 81102 micros/call,
**486612 micros maximum reserved**, within the 5000000 authorized ceiling.
One journal per live run; no retry, fallback, repair or resume after protocol STOP.
Actual account billing will not be inferred from token-price estimates.

## CI classification

The `4a65bcd...` Rust CI run `36498753745` was compared with exact base
`d19be91...` run `36497345650`. Windows Unix API, macOS portable/hosted and Ubuntu
hosted Python/Node failures match. A separate Ubuntu browser-host loopback CDP
timeout is **not baseline-reproduced**. Browser/core Rust sources are unchanged
by #1431; it is retained as an unrelated/unclassified timing observation, not
silently labelled pre-existing. Current exact-head GitHub checks remain the
merge-review source; this ledger does not claim CI green.

## State at this evidence commit

- implemented: yes
- locally verified: yes
- integration verified: actual Runtime/Verifier with synthetic provider only
- merged: no (Draft pending exact-head review)
- deployed: no
- live calls: **0**; spend: **$0**; `DEEPSEEK_API_KEY`: **unread**
- live journal: not initialized
- remote migration: none; #1421 untouched
