# Bounded autonomous Formation exploration — preregistration

This is a new code/policy arm. The historical 6b-E baseline (7/100 typed-K) and
6b-F ledgers are unchanged. The same 100 repository commits, archive hashes,
licenses and source-bound StaticFiles K templates are reused. The previously
authorized explicit K for 81 sources is still a different entry condition from
baseline inference. HTTP 200 alone is not functional acceptance.

Implementation pin: `10473525efa26416ef63025dbb5003c910a43c70`.
Receiver pin: `b20bba7ab9daf282f8d5b1e22affe549ad3492a3`.
Rust receipt authority source: `eb73dff36d15a829e8358d310dbb4106562b344d`.
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

Small acceptance completed: two actual HTTP dependency fixtures reached same-K via four live CandidateProducer calls; actual fixed Runtime cases verified authority repair, out-of-ceiling refusal, fresh reduction, failed-reduction best retention, default/configured rounds, provider cap, no progress and restart/UNKNOWN. WBO and copyparty upstream pilot failures remain failures; no fixture is counted among the 100 OSS. Total small calls are 10 CP and zero DP, all settled.

The first 100 arm uses one frozen binary/receiver set, unchanged prompt/ceiling and all frozen sources. Typed source-limit/path/entry refusals are recorded as actual source-entry terminals with K unformed and no model call; digest/I/O/harness errors stop as infrastructure. Whole-wave journals are validated by the product Rust accounting parser before each new search; call caps and worst-case reservations include all owned pilot and gate runs.

Source transport infrastructure correction after indices1–16: the synthetic owner's existing immutable-source quota is16. Index17 was explicitly rejected before Search creation (SQLite row count0), with no provider reservation/round/attempt. The original D1/R2 and its completed searches remain intact. Subsequent index ranges17–32,33–48,49–64,65–80,81–96,97–100 get isolated receiver storage namespaces with identical checked receiver code/schema/quota, same logical local Runtime/profile, same binaries and sandbox. No quota or product permission is raised; no evidence is deleted. The uncreated search keeps its identity, K, provider binding and zero-use journals; a source transport-only SDK helper submits it once, after which the unchanged CLI resumes normally. Any opened-round expiry during recompilation remains consumed. Completed searches are never rerun or reset.
