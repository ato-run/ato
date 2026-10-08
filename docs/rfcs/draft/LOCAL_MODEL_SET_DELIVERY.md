# Offline local Model Set delivery

Status: draft implementation contract, 2026-10-08. TODO #140 under #136.

The CLI and Connected Worker reuse `ato-runtime-attempt::data_plane::ModelCache`.
`ato model-set import` accepts the existing canonical `ato.model-set/1` manifest,
its declared digest, an existing source directory and a logical cache ceiling.
It checks the manifest and unique required bytes before creating a cache, then
resumes object copies, verifies full SHA-256 and seals them read-only. It never
changes K/D, removes Instance data or obtains provider credentials.

Bounded imports hold an exclusive cache mutation lock; ordinary Worker delivery
holds a shared lock plus one exclusive digest lock. Different digests retain
parallel delivery. A competing writer fails promptly with `model_cache_busy`.
The ceiling counts objects and partial transfers, excluding markers/range
control files and Instance input/output. It is a logical cache limit, not an
entire-volume quota. There is no automatic eviction. POSIX owner/mode enforcement
is required; Windows fails before directory creation.

Only the validated portable LocalProcess executor opts into common Model Set
admission. Formation and Runtime Network entries retain their default refusal.
The executor verifies the complete declared closure from the existing cache
without download, deletion or repair. Missing, malformed or corrupted objects
refuse before launch. Reserved input names and environment-name collisions are
rejected before constructing delivery trees.

Read-only hard links are materialized outside writable process scratch. Linux
stateful routes additionally use read-only bubblewrap mounts. The process sees
the protocol's `ATO_INPUT_PATH_*` environment variables, never the private cache
or marker root. Confirmed stop removes runtime input links, preserving cache
objects. Cache and runtime tree must share a filesystem for hard links.

Host-conditioned/GPU routes remain refused by the common gate before authority
checks. An executor's Model Set delivery does not grant devices, effects,
network access or permission to execute a different D. CPU execution is never
a fallback for a declared GPU D.

An actual macOS process fixture imports the model, removes its upstream source,
reads the model in the sandbox, attempts a denied write and satisfies the frozen
HTTP body digest. This establishes local process delivery for that host only.
GPU process/OCI assignment, exclusive allocation, eviction policy, output-save
retry and real RunPod acceptance remain separate roadmap requirements.
