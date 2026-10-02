# Runner Requirement and Host-Boundary Execution

Status: draft implementation contract  
Date: 2026-10-02  
Tracking: ato-api `docs/rfcs/draft/runner-provisioner.md`,
`docs/rfcs/draft/managed-data-plane.md`

## Decision

1. A Derivation may state the host resources it needs as a **Runner
   Requirement**. It is an admission condition of that Derivation. It is not
   part of K, of `ContractRef`, or of the Application's identity.
2. A Runner reports **measured host resources** in its heartbeat. The control
   plane compares requirement to measurement. Advertised capability strings are
   not used for numeric comparison.
3. A Runner on a host that cannot create namespaces may execute a `process`
   realization in **host-boundary mode**: the machine is the isolation
   boundary, and the Coordinator guarantees the machine serves one Run at a
   time for one owner. This is a distinct, weaker isolation capability and is
   never advertised as `isolation=untrusted-v1`.
4. A Runner can start from a single-use enrollment token.
5. Large objects are streamed to disk and verified by digest before use.

None of this adds a Semantic Core primitive, an Application kind, or a
provider-specific concept. GPU is a host resource, in the same sense that
Python and OCI are Adapter implementations.

## 1. Runner Requirement

### 1.1 Where it lives

`BoundDerivation` gains one additive, optional field, omitted from canonical
JSON when absent:

```json
"runner_requirement": {
  "os": "linux",
  "arch": "x86_64",
  "accelerators": [{ "kind": "nvidia", "count": 1, "min_vram_mib": 16000 }],
  "min_memory_mib": 65536,
  "min_scratch_mib": 102400,
  "runtime_features": ["accelerator=nvidia-cuda"]
}
```

The numbers above are the first measured floor for Wan2.2-Animate with
block swap (peak 14.9 GiB VRAM, 47 GiB host RSS, 45 GiB disk; 2026-10-02).

Every Derivation formed before this contract keeps its exact bytes and
`DerivationRef`. A regression test pins an existing pair.

The requirement is part of the Derivation because it is a fact about *that way
of running* the Application. Two Derivations of one Contract may differ in it;
the `ContractRef` does not change, and the set, order and success of D remain
outside `ContractRef`.

### 1.2 What it must not contain

Provider, region, GPU model name, price, budget. Those are placement policy and
belong to the Coordinator. A requirement names quantities a host either has or
does not have.

### 1.3 Admission

Admission is evaluated for the selected D only. A host that does not satisfy
the requirement is `runner_requirement_unsatisfied`, reported with the first
failing dimension. It is not a bundle validation failure and is never reported
as tampering. A missing capability for a D that was not selected does not
reject the D that was.

## 2. Measured host resources

The heartbeat gains a structured, optional `host_resources` object:

```json
{
  "probed_at": "…",
  "arch": "x86_64",
  "memory_mib": 46000,
  "scratch_mib": 140000,
  "accelerators": [{ "kind": "nvidia", "vram_mib": 46068, "driver": "550.144.03" }]
}
```

Rules:

- Values come from a probe at Runner start, not from configuration. Memory is
  the effective limit of the Runner's cgroup when one is readable, else the
  operator-supplied limit; **host totals are not reported**. (On a RunPod pod,
  `/proc/meminfo` shows the host's 503 GiB, not the pod's allocation.)
- `scratch_mib` is free space on the work root at probe time.
- An accelerator is reported only if a real device query succeeded.
- Absent object = legacy Runner = satisfies no requirement that names a
  quantity.

`accelerator=nvidia-cuda` is added to capability strings only when the probe
found a usable device and the driver library is loadable.

## 3. Host-boundary execution mode

### 3.1 Why

Measured on a RunPod pod (2026-10-02): no `CAP_SYS_ADMIN`; `unshare` of user,
mount, pid, net, ipc and uts namespaces all fail; `mount` fails; `bwrap` fails
in every configuration; no container engine. Landlock (ABI 4),
`no_new_privs`, seccomp and uid changes work. GPU devices and driver libraries
are present in the container.

So neither the bwrap `process` launcher nor the `ato.oci@1` evaluator can run
there. The machine itself is the only strong boundary available.

### 3.2 Contract

Capability: `isolation=host-boundary-v1`. A Runner advertises it only when it
was started with `--isolation host-boundary` and the Landlock probe passed.
It never advertises `isolation=untrusted-v1` at the same time.

In this mode a `process` realization is launched with:

- a dedicated unprivileged uid and gid, distinct from the Runner's;
- `no_new_privs`;
- a Landlock ruleset: read-only on the workspace, the toolchain root and the
  system library paths; read-write only on the declared state mount targets,
  the Run's scratch directory and the model cache mount; no access to the
  Runner's work root, credentials file or environment file;
- a seccomp filter denying namespace, mount, module and `ptrace` syscalls
  (new in this mode; the bwrap path has no seccomp filter today);
- an environment built from the launch spec only;
- its own process group, so teardown kills the whole tree.

Mount semantics differ from the bwrap path because nothing can be mounted:
`/app` and `mount_target` paths are realized as real directories owned by the
workload uid. A spec whose mount targets collide with host paths is refused.

### 3.3 What it does and does not buy

It keeps an honest workload from touching the Runner's files. It does **not**
contain a hostile workload: there is no pid, network or mount isolation, and a
kernel or driver escape reaches the whole machine. Safety comes from the
Coordinator's guarantees, which are part of this contract:

- the machine was created for this owner and is destroyed after use;
- one Run at a time (`max_slots = 1`);
- the Runner's token is revoked with the machine;
- the Run's Data Grant covers only that Run's data.

A lease for a host-boundary Runner that would violate any of these is refused
by the control plane, not by the Runner.

### 3.4 Receipts

The Run receipt records `isolation = "host-boundary-v1"`. Verification is
unchanged: the same authority compares the Run's real observations with K.

## 4. Enrollment start

`ato-connected-realization-worker` accepts `--enrollment-token` /
`ATO_RUNNER_ENROLLMENT_TOKEN`. When set and no credentials file exists, the
worker calls `POST /v1/runners/enroll`, writes the returned runner id and token
to the credentials file with mode 0600 owned by the Runner uid, and proceeds as
today. The enrollment token is not written to disk and is removed from the
process environment before any workload starts.

A worker started with both a credentials file and an enrollment token uses the
file and ignores the token.

## 5. Large object delivery

The worker's `download(&str) -> Vec<u8>` is unsuitable for model weights. A
second path is added, used only for Model Objects and Run input Assets:

- request signed URLs from the lease's Data Grant endpoint;
- download with range requests to a temporary file in the cache directory,
  resuming on failure and re-requesting URLs that expired;
- verify `sha256` and size; rename into the cache only on success;
- expose the cache to the workload read-only at the path the Model Set
  manifest names.

The cache is keyed by digest, is safe to delete at any time, and is never the
source of truth. Outputs are uploaded by multipart from disk and completed
through the grant endpoint before the Runner reports the Run finished.

## 6. Watchdog

A host-boundary Runner started with `--self-stop-command` runs that command
after it has had no successful Coordinator contact for the configured grace
period and no workload is running. The command is supplied by the Runner image
(for example, a provider's "stop this machine" call using a credential scoped
to that machine). The Runner does not know which provider it is on.

## 7. Compatibility

- Additive fields with `#[serde(default)]`; older Runners that refuse unknown
  fields are protected by capability gating in the control plane.
- No change to the bwrap or OCI paths.
- No change to wire v2 or to `ato.portable-application/1` identity rules.

## 8. Acceptance

1. A Derivation with and without `runner_requirement` yields the same
   `ContractRef`.
2. A Runner without an accelerator is not selected for a D that requires one,
   and the Run reports `runner_requirement_unsatisfied`.
3. On a host with no namespace support, a `process` realization runs in
   host-boundary mode, cannot read the Runner credentials file, and its whole
   process tree is gone after stop.
4. The same host refuses to start if asked for `isolation=untrusted-v1`.
5. A 20 GiB object is delivered with bounded Runner memory, survives one
   interrupted transfer, and a corrupted cache entry is detected and refetched.
6. A Runner started from an enrollment token claims its Run's lease, and the
   token does not appear on disk or in the workload's environment.

## 9. Non-goals

- GPU sharing between Runs, MIG, fractional GPUs;
- containment of hostile workloads on hosts without namespaces;
- interactive Surface exposure from on-demand Runners (separate work).

## 10. Open questions

1. Whether `runner_requirement` belongs on `BoundDerivation` or on a
   per-`BoundStep` field, for groups whose services need different hosts. v0:
   Derivation-level, single host.
2. Authoring syntax in `ato.capsule/2`.
3. How the effective memory limit is obtained on providers that expose neither
   a cgroup limit nor an API for it.
