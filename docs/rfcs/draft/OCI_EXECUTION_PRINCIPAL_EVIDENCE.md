# OCI execution principal evidence

Status: Draft implementation note for API#783/#790. Physical evidence only;
no Capsule, Contract, Derivation, launch-spec or authorization schema changes.

The existing Docker startup network inspection also reads the exact container
ID and init PID. On a native Linux host, bounded procfs reads collect real,
effective, saved and filesystem UID/GID plus supplementary groups. Evidence is
emitted only when the PID cgroup names that exact container, both UID/GID maps
are the host identity mapping and process start ticks agree before and after.
No extra Docker call or workload exec is used. Each procfs input is at most8KiB.

The optional principal object records source=linux_proc_status,
observed_stage=container_start and uid_namespace=runner_host_identity_mapping.
It is a startup observation, not a guarantee about every later child process.
Root is representable as an observed fact. An absent/oversize/contradictory read,
remote daemon, remapped namespace or non-Linux host yields no principal; image
USER labels and mount ownership are never substituted for missing observation.
No procfs text, management credential, host path or PID enters K or D.

A Connected Runner includes the object in existing lease execution evidence,
per service for an OCI service group. The API validates it strictly at ready and
portable receipt disclosure, and preserves it under the same authenticated
Runner/lease. TypeScript does not judge Contract satisfaction from credentials.
The curation verifier separately compares the immutable image, platform and
all four UID/GID values with an explicit reviewed fixture; root supplementary
groups, missing evidence or a different fixture remain a hold. Existing Rust
Contract satisfaction and exact Run/lease/attempt checks are also required.

Deploy the optional-field API reader before any Runner emitting the field.
No flag is inferred and no shared Runner deployment is performed by this note.
Real Linux observations, current app startup performance, approved state-owner
initialization and Memos10001/Mealie911 acceptance remain staging gates.
