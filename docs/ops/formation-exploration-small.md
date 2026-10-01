# Bounded exploration: actual provider and Runtime acceptance

Implemented in the unmerged exploration branches; not deployed. The machine
ledger records exact K/D/attempt/receipt, raw evidence hashes and provider facts.
No fixture is an upstream OSS coverage or functional-acceptance success.

Two small HTTP application fixtures reached their frozen K through the actual
DeepSeek adapter, requester, Coordinator, contained process Runtime and Verifier.
Both final submissions have `k_reached_awaiting_assessment`,
`approval=not_assessed`, and zero normal `verified_routes`.

| Gate | Actual observation | Evidence kind |
| --- | --- | --- |
| No known D | Live LLM generated locked npm install + Node startup, fresh same-K PASS | actual provider / Coordinator / Runtime |
| Missing dependency | Declared fixture route failed with `ERR_MODULE_NOT_FOUND`; live LLM proposed npm ci and dependency-phase registry egress; fresh PASS | actual provider / Coordinator / Runtime |
| Authority repair | Missing logical bind rejected; changed canonical D restored bind and passed | fixed typed producer / actual Runtime |
| Outside ceiling | Proposed production resource retained as evidence; `exploration_authority_exceeded`, zero attempts | fixed typed producer / actual Coordinator |
| Permission reduction | Removed unused build-phase registry requirement; new D and fresh receipt passed | fixed typed producer / actual Runtime |
| Failed reduction | Missing bind rejected; previous successful D/receipt retained | fixed typed producer / actual Runtime |
| Default / configured rounds | Default 3; configured 1 and 5 stopped at exact limits | actual Coordinator |
| Independent provider cap | Original max-rounds=5/provider-cap=4 search resumed unchanged and stopped at 4 with `budget_exhausted` | actual Coordinator, no reset |
| No progress | Duplicate D rejected after one attempt; stop `no_progress` | actual Coordinator |
| Restart | Coordinator and Runtime stopped/restarted; frozen inputs, round/attempt budget and best submission unchanged | actual product restart |
| UNKNOWN | Runtime attempt-record write was refused; UNKNOWN survived restart and did not invoke a producer or repeat execution | actual worker journal failure |

Live fixture minimization proposals did not establish a reduced receipt. Their
initial successful D remained the submitted candidate. The successful reduction
above is a separate fixed-proposal Runtime gate; these evidence types are not
combined or represented as an LLM reduction success.

Frozen upstream pilots were WBO (index 2) and copyparty (69), three live calls each.
Neither reached K. WBO's first D lacked state authority, the subsequent proposal
added it and reached actual npm install/process startup, but startup did not become
observable. Copyparty's failed script startup produced import evidence; a later
proposal reached module startup but did not pass. Their failures stay failures.
No prompt, source or ceiling was changed in response to those application results.

There were ten live CandidateProducer calls in the entire small stage, zero
DecisionProvider calls, 26,613 USD micros estimated from known usage and 98,310
micros conservatively charged from the frozen per-call reservations. All model
reservations settled. Earlier no-call infrastructure pilots are preserved,
including their original consumed rounds and deadlines.

The 100 OSS arm uses its own preregistered immutable code/binary/receiver set.
Historical baseline, former adaptive arm, static functional evidence and source-OCI
capability observations remain separate. No new browser/API functional acceptance,
production access, deployment or remote migration occurred.
