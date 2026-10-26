# Dedicated GPU Formation runtime

Implementation work; no paid GPU was created for this change and real GPU acceptance remains pending.

The CPU Formation worker publishes the validated immutable candidate to quarantine before refusing a GPU route with `gpu_host_unavailable`. It does not publish a satisfied result. A separately authorized, job-bound GPU worker rebuilds the same candidate, uses the existing portable runtime authority, and returns an ordinary FormationRuntime receipt without a Run identifier.

The connected worker supervises one Formation child and advertises no ordinary Run lease kinds. The child requires an explicit host boundary with a separate workload UID/GID, Landlock, and no_new_privs. Model inputs use the existing verified ModelCache and attempt-scoped range delivery; cancellation revokes subsequent delivery requests. Provider keys are never passed to the child or workload. Completion returns control to the existing Pod deletion wrapper.

The shared attempt admission remains closed for host requirements unless its executor explicitly measures them. Dedicated portable Formation measures the GPU, memory limit, and scratch; this admits no additional network, filesystem, or other effect authority. Automatic Search retains its refusal. The initial supported GPU realization is an authored portable v2 process without namespace-dependent state mounts.

The API must account for immutable model cache bytes in addition to its usual container disk floor and declared scratch. The existing storage price ceiling still applies. This physical capacity projection does not rewrite the sealed Contract or Derivation.

Local checks: Formation library 173 tests; measured-host unit tests 12; worker role and incomplete sandbox identity tests; actual bundle quarantine followed by a GPU admission refusal; common admission regression tests. These are free tests, not real GPU acceptance.

## Staging rollout and live CPU evidence

The clean Linux build at `a9c39128938b4a0527709d7e8ba351db9bc6d49d`
(build ID `formation-gpu-a9c39128`) was published and hash-verified:

- Formation worker: `3f9fbc6b8d7de67a79a8e0b588e0d19d1f94ed36eafce57e647d9ca8795b34b6`
- Connected worker: `912509fffe5c6dcc70477008c058d88d3130e66985143df52dc0a0d7a9b83212`

Both staging CPU Formation units on ubuntu-sugamo were gracefully stopped
(exit 75) and started with the candidate plus an expected digest. The managed
worker heartbeat reports the matching clean build and active lifecycle. The
production unit, old binaries and lifecycle history were preserved.

Actual PWA intake of the approved eight-file Wan sample produced the same
previously accepted bundle (`929aa3dc…`), a typed `gpu_host_unavailable` refusal
and no successful CPU Formation result. The API reported GPU verification
available. The PWA then displayed explicit operator/owner RunPod choices for
verification, with a separate choice required for later app execution. Only
Cancel was selected; no GPU provision or binding was created.

This validates source intake, canonical candidate preservation, CPU refusal,
configuration and UI cancellation. GPU model delivery/execution/cleanup still
require paid-machine acceptance and were not claimed as verified here.
Full deployment and reconciliation record lives in ato-api
`docs/ops/formation-gpu-staging-2026-10-26.md` (PR #902).
