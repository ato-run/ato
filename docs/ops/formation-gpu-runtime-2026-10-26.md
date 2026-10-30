# Dedicated GPU Formation runtime

Implementation work; no paid GPU was created for this change and real GPU acceptance remains pending.

The CPU Formation worker publishes the validated immutable candidate to quarantine before refusing a GPU route with `gpu_host_unavailable`. It does not publish a satisfied result. A separately authorized, job-bound GPU worker rebuilds the same candidate, uses the existing portable runtime authority, and returns an ordinary FormationRuntime receipt without a Run identifier.

The connected worker supervises one Formation child and advertises no ordinary Run lease kinds. The child requires an explicit host boundary with a separate workload UID/GID, Landlock, and no_new_privs. Model inputs use the existing verified ModelCache and attempt-scoped range delivery; cancellation revokes subsequent delivery requests. Provider keys are never passed to the child or workload. Completion returns control to the existing Pod deletion wrapper.

The shared attempt admission remains closed for host requirements unless its executor explicitly measures them. Dedicated portable Formation measures the GPU, memory limit, and scratch; this admits no additional network, filesystem, or other effect authority. Automatic Search retains its refusal. The initial supported GPU realization is an authored portable v2 process without namespace-dependent state mounts.

The API must account for immutable model cache bytes in addition to its usual container disk floor and declared scratch. The existing storage price ceiling still applies. This physical capacity projection does not rewrite the sealed Contract or Derivation.

Local checks: Formation library 173 tests; measured-host unit tests 12; worker role and incomplete sandbox identity tests; actual bundle quarantine followed by a GPU admission refusal; common admission regression tests. These are free tests, not real GPU acceptance.
