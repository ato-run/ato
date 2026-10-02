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

That proof establishes attempt/report success, not completed Search selection:
the existing exploration policy permits a further strict requirement-reduction
round. A separate Requester resume opened that round; its initial read-only
assertion correctly refused to label it an inference-free recovery. No Runtime
execution or session response was supplied by that failed controller. The original
failure logs, pending exchange and limits are preserved. Completion and functional
acceptance are recorded separately once observed.

This change adds no paid API call, remote migration, deployment, feature flag,
normal Run authorization or public artifact publication. Old WBO UNKNOWN remains
unchanged. The new retry code has not yet been exercised by that prior trial;
its Linux final-pin fault/application acceptance remains required.
