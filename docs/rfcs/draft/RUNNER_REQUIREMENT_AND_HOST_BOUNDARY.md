# Runner Requirement and Host-Boundary Execution

Status: draft implementation contract  
Date: 2026-10-02  
Tracking: ato-api `docs/rfcs/draft/runner-provisioner.md`,
`docs/rfcs/draft/managed-data-plane.md`

## Decision

1. A Derivation may state the host resources it needs as a **Runner
   Requirement**, carried in the existing `BoundDerivation.requirements`. It is
   an admission condition of that Derivation. It is not part of K, of
   `ContractRef`, or of the Application's identity. It is stated per
   Derivation, never per step: **one `BoundDerivation` runs on one Runner.**
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

`BoundDerivation` already has `requirements: ExecutionRequirements`
(`lib/formation/src/requirements.rs`), holding `network` and `authority`. Host
resources join it as one additive member, `host`, omitted from canonical JSON
when absent. No new field is added to `BoundDerivation` itself.

```json
"requirements": {
  "host": {
    "os": "linux",
    "arch": "x86_64",
    "accelerators": [{ "vendor": "nvidia", "count": 1, "min_vram_mib": 45000 }],
    "min_memory_mib": 65536,
    "min_scratch_mib": 102400,
    "runtime_features": ["accelerator=nvidia-cuda"]
  }
}
```

`BoundStep` does not get a requirement. A route that needs different hosts for
different steps (download on CPU, inference on GPU, encode elsewhere) is not
expressed by adding a per-step requirement: that would turn placement into a
distributed scheduler. It is a Composite Run of sub-Derivations, each of which
again runs on one Runner, and is out of scope here.

The numbers above are the **verified baseline** for Wan2.2-Animate: the one
configuration that was actually run (NVIDIA A40, 46 GiB VRAM; 47 GiB host RSS
peak; 45 GiB disk used; 2026-10-02, see ato-api
`docs/ops/open-compute-wan-phase0-2026-10-02.md`). 64 GiB RAM and 100 GiB
scratch are initial candidates with headroom, not measured minimums.

Every Derivation formed before this contract keeps its exact bytes and
`DerivationRef`. A regression test pins an existing pair.

The requirement is part of the Derivation because it is a fact about *that way
of running* the Application. Two Derivations of one Contract may differ in it;
the `ContractRef` does not change, and the set, order and success of D remain
outside `ContractRef`.

### 1.1.1 Verified baseline versus unverified profiles

A requirement states what a Derivation was shown to run on. Smaller hosts are
separate profiles and are unverified until run:

| Profile | Status |
|---|---|
| 48 GiB-class GPU (A40), block swap 25 | verified |
| 24 GiB GPU, no block swap | unverified; peak was 23.5 GiB on the A40, no headroom shown |
| 16 GiB GPU, block swap 25 | unverified; a 14.9 GiB peak on a 46 GiB card does not show a 16 GiB card works |

A profile is promoted by running it, not by reading a peak from a larger host.

### 1.1.2 Placement conditions versus execution-path conditions

Two different things must not be mixed:

- **Placement conditions** — accelerator class, VRAM, RAM, scratch. They are
  the Runner Requirement and select a host.
- **Execution-path conditions** — the pinned software the Derivation runs:
  ComfyUI, ComfyUI-WanVideoWrapper and the other node packages, PyTorch, the
  CUDA libraries, ONNX Runtime, and the Model Set revision. They are pinned by
  the Derivation (Runner image digest, dependency hashes, Model Set digest) and
  are the same on every host.

For the measured configuration, ComfyUI `65787d6` started with PyTorch
2.8.0+cu126 and did not start with 2.4.1 or 2.6.0. That is a statement about
this pinned set, not a compatibility claim about other versions.

Pose detection ran on CPU in the measurement (the installed ONNX Runtime GPU
build could not load its CUDA provider). Moving it to GPU is a separate
improvement and is accepted only with evidence that the provider loaded.

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

The heartbeat gains a structured, optional `host_resources` object. Every
value has a named source, and a value with no trustworthy source is reported
as unknown, never guessed.

```json
{
  "probed_at": "…",
  "arch": "x86_64",
  "memory": { "cgroup_limit_mib": null, "proc_meminfo_mib": 515000 },
  "cpu": { "cgroup_quota_millis": null, "online": 112 },
  "scratch_mib": 140000,
  "accelerators": [{ "vendor": "nvidia", "vram_mib": 46068, "driver": "550.144.03" }]
}
```

| Quantity | Source | Notes |
|---|---|---|
| memory | cgroup limit: v2 `memory.max`, else v1 `memory.limit_in_bytes`; a finite value is the container's hard limit | `max`/unreadable → `null` |
| CPU | cgroup quota first (`cpu.max`, or v1 `cfs_quota_us`/`cfs_period_us`) | online CPU count is diagnostic |
| GPU VRAM | NVML / `nvidia-smi` for a device that answered | never from configuration |
| scratch | `statvfs` on the Runner's actual work root | free space at probe time |

`/proc/meminfo` is carried as a **diagnostic only**. It is never used for
admission. On a RunPod pod it reports the host's 503 GiB, and the cgroup files
were not readable.

### 2.1 Effective memory

The Runner holds no provider credential, so it cannot ask the provider what it
was allocated. The Coordinator already knows: the backend reports the
allocation when it creates the machine (`provider_memory_mib` on the provision
row). Admission therefore computes, on the control plane:

```
effective_memory = min( finite cgroup limit reported by the Runner,
                        provider-reported allocation )
```

using whichever of the two exist. **If neither exists, memory is `unknown`,
and `unknown` does not satisfy any `min_memory_mib`.** A Runner that can show
only `/proc/meminfo` is not admitted for a Derivation that states a memory
floor. The same rule applies to every quantity: absent or unknown never
satisfies a stated floor.

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

- the machine is pinned to one owner when it is created and is never assigned
  to another during its life; cross-owner reuse is forbidden in v0;
- one Run at a time (`max_slots = 1`);
- between Runs the Runner deletes the previous Run's inputs, outputs and
  temporary workspace; only the digest-verified model cache survives;
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

## 6. Loss of Coordinator contact

A Runner that cannot renew execution authorization tears its workload down
(existing behaviour), deletes the Run's files, and stops polling. It holds no
provider credential and does not know which provider it is on, so it cannot
delete its own machine. Removing the machine is the Coordinator's job
(reconcile, every minute), and a provider-side hard TTL set when the machine is
created ends it even if the Coordinator is gone; see the Runner Provisioner RFC
§4.6.

## 7. Compatibility

- Additive fields with `#[serde(default)]`; older Runners that refuse unknown
  fields are protected by capability gating in the control plane.
- No change to the bwrap or OCI paths.
- No change to wire v2 or to `ato.portable-application/1` identity rules.

## 8. Acceptance

1. A Derivation with and without `requirements.host` yields the same
   `ContractRef`, and one without it keeps its exact bytes and `DerivationRef`.
2. A Runner without an accelerator is not selected for a D that requires one,
   and the Run reports `runner_requirement_unsatisfied`. A Runner whose memory
   is `unknown` is not selected for a D that states a memory floor.
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

1. Authoring syntax in `ato.capsule/2`. The first end-to-end path needs only
   the bound form of `requirements.host`; the authoring spelling stays a review
   item unless forming the Wan application requires it.
2. Whether RunPod injects a pod-scoped credential into the container; if so it
   must be kept from the workload (Runner Provisioner RFC §10).

Resolved 2026-10-02: the requirement is Derivation-level and one
`BoundDerivation` runs on one Runner (§1.1); memory admission uses the cgroup
limit, then provider metadata, and `/proc/meminfo` only as a diagnostic (§2).
