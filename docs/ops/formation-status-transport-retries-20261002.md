# Durable retry limits for Requester status reads

Base: Ato `87501e2fc88cd986759ba403dc4e60db53b31c57` (application code
`3ac36fa2739cf8d9555026e93a4ca79905a6d4a7`), API
`fa825a176b715962351466dc1a7149bec5979c7f`. Receipt authority is unchanged.

In changedetection.io trial 14, the original Runtime completed native operations
and fresh frozen-K verification, then acknowledged its owner-private BuildRecord.
The Requester's direct status GET lost its connection and exited. The acceptance
controller stopped its Runtime before retained publication completed. This was a
status transport failure, not evidence that the source or native operations failed.

The shared Codex/API reasoning Requester now uses the original Search's custom
`max_retries` for every logical status observation. Durable dispatch reservations
precede sends; reopening the same failed observation preserves consumed retries,
identity, deadline and retry policy. A completed observation stores only its small
delivery acknowledgement. The next poll and a reopened Requester fetch a fresh
status, rather than replaying a stale cached response. Observations may continue
after the execution deadline using the existing bounded reporting HTTP client;
submission, claims and inference remain constrained by the original deadline.
Transport retries do not issue a provider call or consume a Formation round.

Verification: Worker library 75 PASS. The added actual loopback HTTP fixture drops
the first response, checks custom one-retry recovery, reopens the producer and
observes a different status. The durable error test proves initial plus one sends,
zero additional dispatches after restart/exhaustion, and refusal to change the
retry limit or operation identity. Formatting and changed-package/all-target
clippy with `-D warnings` PASS. The first clippy run rejected an unused test-fixture
read amount; the fixture now reads bounded complete HTTP headers. No production
behavior was skipped to obtain a pass.

The unchanged trial-14 Runtime subsequently resumed its confirmed publication
checkpoint, without source acquisition/build/launch/inference, and delivered its
original PASS to the Coordinator. BuildRecord
`sha256:4b1e581775cc6e8a59a6bae4c2f82b424120f8ba1dd621e7168c3e9c95166ce8`
and retained candidate
`sha256:5a4563db6cd7a412c84ba31e2bcf123eff81be06d1514be61409fabf133a5f0d`
are Ready. Combined stored usage is 306,002,627 bytes. The before/after Search
limits and deadline are identical. Recovery proof SHA-256:
`1d35bf1939542c65251eb3d92f08169445c881502353121988655edf7f385a28`.

That proof establishes attempt/report success, not completed Search selection.
The existing policy permits a strict requirement-reduction round. A separate
Requester resume opened that round; its read-only assertion correctly refused
to label it an inference-free recovery. The failed controller supplied no session
response or Runtime execution. A separately recorded Codex answer to the existing
common input then rejected unsupported reduction without another execution.
Original Search `search_fa9b884b3a7c0f73836ae5fc6bedf05f` became satisfied,
two rounds / three session exchanges / one Runtime attempt. Old failure logs
and original limits remain preserved. This source Runtime used `3ac36fa2`.

Actual Linux final-pin acceptance at code
`1de0e3a008f73226171d83fa026c50ec20dde976`, API `fa825a17`, unchanged
authority WASM source `3ac36fa2`, completed with zero known D:

- SVGOMG trial 16: a transparent proxy dropped one real Coordinator status
  HTTP 200 response after the backend completed. Custom `max_retries = 1`
  recovered with exactly two durable sends, delivery-acknowledgement-only cache,
  unchanged Search deadline/limits and no extra round for transport retry.
  Source-evidenced correction of HTTP authority and required es5-ext lifecycle
  produced fresh same-K PASS. Three rounds / four session exchanges / three
  claimed attempts, including one admission refusal and two actual executions.
  Original SVG trial 15 had an authoring schema error, zero Runtime attempts;
  preserve its no_progress result separately.
- Single-service OCI trial 15: root Dockerfile, first generated D, normal Runtime
  launch, fresh unchanged-K PASS and submitted Search. Two rounds / two session
  exchanges / one execution. This fixture is excluded from OSS unique-app counts.

Proof `status-retry-final15-16-proof.json` SHA-256
`91f5474fbe2d469fffd84b723bf53443ad95c1c6037d960f11a0449c0d112a70`.
Both successful Searches await assessment, with zero outstanding reservations.

This change adds no paid API call, remote migration, deployment, feature flag,
normal Run authorization or public artifact publication. Old WBO UNKNOWN remains
unchanged. API-provider acceptance and the remaining whole-app/state/input gates
remain required. The skip-ci head has no executed CI result; it is not CI PASS.
