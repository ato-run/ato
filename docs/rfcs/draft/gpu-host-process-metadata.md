# GPU host process metadata writes

Status: draft; staging implementation and real CUDA validation in progress.

## Observed requirement

The host-boundary Runner on an A6000 with NVIDIA 595.91.07 loads the actual
driver and opens the NVIDIA devices read/write as UID/GID 20001, with no
effective capabilities, no_new_privs and Landlock. CUDA cuInit nevertheless
returns 304. A trace of a CUDA-only child shows the denied call:

```text
openat(..., "/proc/self/task/<tid>/comm", O_WRONLY|O_CREAT|O_TRUNC, 0666)
    = -1 EACCES
```

The earlier A40/570.195.03 Run has the same CUDA error, but was not traced.
This is a driver realization requirement, not a Wan application special case
or a change to the Capsule, Contract, Derivation, software, or model identity.
NVIDIA describes the same issue in
[OpenShell #1486](https://github.com/NVIDIA/OpenShell/issues/1486). Its
[narrow-permission follow-up](https://github.com/NVIDIA/OpenShell/issues/1628)
also notes that /proc/self/task does not cover descendant processes.

## Policy

Extend the existing sandbox policy with an explicit, serialized
`file_write_paths` list. Linux grants only WriteFile and Truncate below those
paths, in addition to any separately declared read rights. It grants no
creation, removal, rename, execution, or new read rights. Required rules fail
closed if their path cannot be opened or their rule cannot be installed.
Other platforms refuse this permission, rather than silently widening it.
Deserializing an older policy leaves the list empty.

For host-boundary launches, the existing physical device binding enumerates
visible NVIDIA character devices. When that binding exists, add /proc to
file_write_paths; CPU-only hosts receive no rule. This applies to each Run on
that GPU host, including CPU processes launched there; it does not claim to
be conditioned on an accelerator field in RuntimeLaunchSpec, which currently
does not carry such a field. Namespace execution remains unchanged.

The effective serialized policy and a structured Linux rule log expose the
addition as `write_file,truncate`. This is broader than comm alone: Landlock
has no procfs wildcard rule covering dynamic descendant PIDs/TIDs. Ordinary
UID/DAC checks still constrain procfs operations; do not claim that only
thread-name files can be modified. System control files and the root Runner's
procfs memory/credentials stay subject to those checks. The one-owner,
one-active-Run, disposable-machine boundary, non-root workload, cleared
supplementary groups, no_new_privs, required Landlock and denied TCP egress
remain. Never grant /proc full read-write/create/remove/execute rights, or
grant shared temporary directories to accommodate a driver.

## Verification

Run Linux tests in separate child processes, because Landlock is irreversible.
Prove the exact O_CREAT|O_TRUNC thread-name write in the initial process and a
descendant, with WriteFile/Truncate. Prove the baseline denies it. Also prove
file creation/removal/rename, denied-path symlink/procfs-root escape and TCP
bind remain refused. A WriteFile-only prototype failed the comm open on the
test kernel; retain that result rather than claiming Truncate is unnecessary.

On the real GPU, repeat the CUDA-only trace with the compatible artifact,
record actual UID/capability/no_new_privs and driver facts, and require
cuInit=CUDA_SUCCESS plus a positive device count. Only then resume the actual
fixed Wan cold/warm/fresh-Pod generation, saved-output and save-fault recovery
acceptance. Health Contract verification alone is not GPU acceptance.
