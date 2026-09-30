# Bounded autonomous Formation exploration — preregistration

This is a new code/policy arm. The historical 6b-E baseline (7/100 typed-K) and
6b-F ledgers are unchanged. The same 100 repository commits, archive hashes,
licenses and source-bound StaticFiles K templates are reused. The previously
authorized explicit K for 81 sources is still a different entry condition from
baseline inference. HTTP 200 alone is not functional acceptance.

Implementation pin: `7552fcfe714f26ed44bf0b7abae74b61d3651c9d`.
Receiver pin: `6a7fd9dd838ba306a260992dcf82de03233bbd20`.
Rust receipt authority source: `64e03d0eb063eae5a08f516e629d53e26e824414`.
Exact binary/receiver hashes and frozen source identities are in the JSON plan.
No implementation, prompt, ceiling or model change is allowed during the 100 arm.

The CandidateProducer is DeepSeek Flash with prompt v3, disabled thinking and
2048 output tokens. A started round is charged even when declined, invalid,
inspection-only or failed. Default rounds are three; known attempts are separate.
Each search has a durable journal. DecisionProvider Jev 1.13.0 is used only for
an ambiguous finite choice; prescribed and singleton choices do not call it.
Input/cost reservations use peak cache-miss pricing and are conservative.

Maximum reservation is 3,443,244 USD micros: 218,644 for the small acceptance
gate and 3,224,600 for 100 searches. This fits the remaining 3,507,128 micros;
reserved tokens with unknown usage are fully charged and never resent.
No credential values are recorded. Only the requester receives the two provider
keys through sealed RAM, after the complete non-secret gate and preregistration
push. Runtime workloads receive neither provider nor production credentials.

The physical exploration sandbox grants exact HTTPS destinations separately for
dependency/build phases, with no runtime egress by default. Logical HTTP bind and
three named empty filesystem slots are the only runtime authority ceiling.
HTTP port 80, additional endpoints, unbound source-OCI recipes and unimplemented
toolchains are explicit adapter/capability diagnoses. They are not silently
converted into permission. Source archive/expansion/wire limits stay unchanged.

Small acceptance uses frozen WBO (index 2) and copyparty (index 69). Required
repair, refusal, reduction, failure retention, round-limit and restart gates use
actual Coordinator/Runtime; scripted underprivileged D are separate test cases,
not injected into the 100 automatic arm. The 100 arm starts only after these
acceptance gates pass. Credential-free 2048 checks the known-D regression path.
Infrastructure failures may be diagnosed before the live gate; application
failures do not authorize prompt tuning or source rewrites.

Every successful submission is `k_reached_awaiting_assessment`; no ordinary
verified-route permission, publication, deployment or remote migration follows.
Results must report fresh receipt assignment, D/requirements history, reduction
verification, rounds/attempts, provider usage/latency/cost and no-call reasons.
The retained artifacts, frozen archives, journals and receipts are preserved.
No host-wide Docker prune is permitted. `#1421` remains untouched.

Pilot preregistration revision: source-context canonical ordering was repaired before
any model reservation or send. The prior plan and failed infrastructure evidence
are preserved. Continuation reopens the same search/journals and keeps its consumed
round and original deadline; it does not create a fresh search budget.

Restart locator and authenticated HTTP readiness were repaired before any model
reservation/send. Prior journals and original round/deadline remain unchanged.
The preflight helper remains the byte-identical revision-2 helper.

The pilot reproduced a sender-only `proposal_claim_invalid` HTTP 400. Claimants
now use the existing UUID schema. No model send/reservation preceded this repair;
three expired pilot rounds remain in their original ledgers.
