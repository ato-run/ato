# Beyond Reproducible Builds: Execution Identity for Cross-Platform Source-Native Software

## Abstract

Reproducible builds make software artifacts accountable. Given the same source code, build environment, and build instructions, any party should be able to recreate bit-for-bit identical copies of the specified artifacts. This framing has been highly successful for compilers, operating system distributions, package ecosystems, and supply-chain verification. However, cross-platform source-native software distribution requires identifying a different object: not only the artifact produced by a build, but the launch conditions under which source code becomes a running process.

A source tree may build reproducibly yet execute differently across platforms due to runtime resolution, dependency materialization, dynamic libraries, environment leakage, filesystem views, network policy, persistent state, locale, timezone, and entrypoint configuration. Existing systems identify adjacent objects. Reproducible Builds identify artifact bytes. Nix identifies derivations and store outputs. Docker identifies images. Package managers identify dependency resolutions. ReproZip captures post-hoc execution traces. None of these directly identifies the pre-execution launch envelope of source-native software.

This paper introduces **Execution Identity**, a content-addressed representation of launch conditions for source-native software. An execution identity hashes source tree identity, dependency derivation identity, dependency output identity, runtime identity, environment closure, filesystem view, network policy, capability policy, entrypoint, arguments, and working directory. We further introduce **reproducibility classes**—`pure`, `host-bound`, `state-bound`, `time-bound`, `network-bound`, and `best-effort`—to make the causes of cross-platform execution drift explicit.

We implement this model in Ato, a source-native execution runtime for local projects, GitHub repositories, and shared application handles. Ato separates source provenance, materialized source identity, dependency derivation, immutable output blobs, session-local effects, and persistent user state. We show how Execution Identity enables drift detection, launch-envelope replay, non-polluting execution, and auditable cross-platform source distribution.

---

## 1. Introduction

Source code is increasingly distributed as something to run, not merely something to build.

Developers clone repositories and launch demos before reading the full setup instructions. Tools are invoked directly from GitHub. Scripts are generated and executed as part of local workflows. AI coding agents inspect, modify, run, and discard source repositories during a single task. Scientific and data workflows are shared as source trees rather than only as prebuilt binaries. In each case, the user expectation is similar:

```text
Given this source, run the same thing here.
```

But “the same thing” is difficult to define.

The established answer is reproducible builds. A build is reproducible if, given the same source code, build environment, and build instructions, any party can recreate bit-for-bit identical copies of all specified artifacts. The reproducible-builds.org definition explicitly centers artifact identity and verifies reproducibility through bit-by-bit comparison, usually with cryptographic hashes.

This is necessary, but not sufficient.

A reproducible build identifies an artifact. A source-native execution launches a process. A process observes a launch world: the runtime binary selected by PATH or a tool manager, the dependency tree materialized by a package manager, inherited or reconstructed environment variables, dynamic libraries, filesystem mounts, writable state, network restrictions, locale, timezone, command-line arguments, and working directory.

Two machines can hold the same source tree and still launch different executions. Two users can run the same Docker image digest with different bind mounts, environment variables, commands, working directories, and network options. Docker itself defines `docker run` as a composition of options, an image reference, and optional command and arguments, and documents that image defaults such as command, entrypoint, environment variables, user, and working directory can be overridden at run time.

Similarly, Nix provides a rigorous model for derivations and store outputs. A Nix derivation is a build task described by attributes such as system, builder, arguments, environment variables, inputs, and outputs.  But a source-native run is not only a derivation output. It is a launch: selected runtime, dependency projection, filesystem view, environment closure, policy, entrypoint, arguments, working directory, and possibly state binding.

This paper argues that cross-platform source-native software distribution needs a new identity object: **Execution Identity**.

The goal is not to guarantee identical process traces across all platforms. That would collapse into deterministic record/replay, which is a different problem. The goal is to make execution launches accountable:

```text
Build reproducibility identifies artifacts.
Execution Identity identifies launches.
```

When two executions have the same execution identity, they have equivalent launch conditions. Whether equivalent launch conditions produce equivalent behavior depends on the execution’s reproducibility class.

This paper makes three contributions:

1. **Execution Identity.** We define a content-addressed identity for the pre-execution launch envelope of source-native software.
2. **Reproducibility Classes.** We classify executions by the cause of non-reproducibility rather than treating reproducibility as binary.
3. **Ato Reference Runtime.** We describe Ato, a source-native runtime that computes, records, and reconstructs execution identities through layered state, dependency materialization, runtime identity, environment closure, filesystem views, and policy hashes.

---

## 2. Why Build Reproducibility Is Not Execution Reproducibility

### 2.1 Reproducible builds identify artifacts

Reproducible builds ask:

```text
Can the same source, build environment, and build instructions recreate the same artifact bytes?
```

The output object is an artifact: an executable, package, distribution archive, filesystem image, or other specified build result. The success condition is bit-for-bit identity.

This model is powerful because it makes artifacts accountable. It supports independent verification, supply-chain transparency, distribution trust, and debugging of build nondeterminism.

However, artifacts are not executions. A process launch may depend on many conditions outside the artifact itself.

### 2.2 Hermetic builds identify build conditions

Hermetic build systems go further by isolating builds from the host. Bazel describes a hermetic build as one that, given the same input source code and product configuration, always returns the same output by isolating the build from changes to the host system. It treats tools as managed inputs and depends on specific versions of build tools and libraries.

This is closely related, but still build-centered. The identity target is the build action and its output. Execution Identity moves the same discipline to process launch conditions.

```text
Hermetic builds make build actions accountable.
Execution Identity makes launch actions accountable.
```

### 2.3 Nix identifies derivations and store outputs

Nix is the strongest counterpoint to this work. It already provides a rigorous model for build derivations, input closure, immutable store outputs, and cross-platform package construction. A derivation describes a build task, including system type, builder, arguments, inputs, environment variables, and outputs.

This paper should not claim that Nix is “unstable” or “insufficient” in its own domain. It is better to say:

```text
Nix makes build inputs explicit.
Execution Identity makes launch conditions explicit.
```

The difference is the identity target.

A Nix store path can identify a package output. It does not by itself identify an arbitrary source-native launch over:

```text
runtime binary
dependency projection
environment closure
filesystem view
network policy
capability policy
entrypoint
argv
cwd
state bindings
```

Nix can be used to construct many of these pieces. But the launch envelope itself is not the primary identity object.

### 2.3.1 Nix in Practice: Launch Drift After Store Identity

This distinction is not only theoretical. Real Nix deployments expose a class of problems that are better described as launch-envelope drift than as failures of the derivation model.

For example, interactive Nix environments do not necessarily form a fully closed execution environment. The [Nix manual](https://nix.dev/manual/nix/latest/command-ref/nix-shell.html) describes `nix-shell --pure` as clearing the environment "almost entirely," while retaining variables such as `HOME`, `USER`, and `DISPLAY`, and while allowing shell resolution to be affected by `NIX_BUILD_SHELL` and `NIX_PATH`. In practice, users also report wrapper-provided variables such as `PATH`, `LD_LIBRARY_PATH`, and `PYTHONNOUSERSITE` propagating from Nix-built terminal emulators into child shells, and user-level R library paths leaking into Nix R development shells. These are environment-closure issues, not derivation-output issues.

Dynamic library resolution provides another example. The [nix-ld README](https://github.com/nix-community/nix-ld/blob/main/README.md) explicitly avoids directly relying on `LD_LIBRARY_PATH` because it affects all programs and can inject the wrong libraries into otherwise correctly built Nix applications with RPATHs. CUDA illustrates an even stronger host-bound case: `libcuda.so.1` is tied to the host NVIDIA kernel driver, so placing it in the Nix store can create version mismatches when the host driver changes. Recent [PyTorch/CUDA reports](https://github.com/NixOS/nixpkgs/issues/461334) also show runtime compilation failing because `libnvrtc` is not found in the expected runpath, even though a binary package variant works. These cases show that runtime dynamic linkage and host-bound drivers are part of the execution identity.

Filesystem and policy drift also appear in practice. A Makefile that assumes `/bin/cp` may fail under `nix-build` while working under `nix-shell` or a Nix Docker image, because the filesystem view differs. On macOS, Nix builders have historically used weaker sandbox settings because full sandboxing breaks many builds; one [reported issue](https://github.com/NixOS/nix/issues/6049) describes a Hydra macOS builder starting an IPFS server during a build and producing external network traffic, which in turn motivated a stricter network sandbox discussion. A later [Darwin regression](https://github.com/NixOS/nix/issues/11002) also broke many build-triggering commands during sandbox setup. These examples indicate that sandbox strength, network policy, and filesystem view are platform-specific launch conditions.

These observations should not be read as evidence that Nix is weak. On the contrary, Nix provides one of the strongest existing models for derivations, immutable store outputs, and build-input explicitness; large-scale studies report high and improving bitwise reproducibility in nixpkgs. The point is narrower: Nix primarily identifies derivations and store outputs, while source-native execution also needs to identify the runtime, dependency projection, environment closure, dynamic library closure, filesystem view, network policy, capability policy, entrypoint, arguments, working directory, and state bindings.

In this sense, Execution Identity complements rather than replaces Nix:

```text
Nix makes build inputs explicit.
Execution Identity makes launch conditions explicit.
```

### 2.4 Docker identifies images, not full launch envelopes

Docker provides a portable image and container execution model. Docker images have content-addressable digests, and `docker run` can create isolated processes with their own filesystem, networking, and process tree.

However, Docker image identity is not launch identity.

A Docker run is not just an image digest. It is:

```text
docker run [OPTIONS] IMAGE[:TAG|@DIGEST] [COMMAND] [ARG...]
```

The options may configure filesystem mounts, networking, environment variables, working directory, user, resource limits, capabilities, and more. Docker also documents that image defaults can be overridden at run time, including command, entrypoint, environment variables, user, and working directory.

Therefore:

```text
Docker image digest identifies the image.
Execution Identity identifies the launch.
```

### 2.5 Package managers identify dependency resolutions

Package managers and lockfiles identify dependency choices within ecosystems. They are essential, but incomplete. A lockfile usually does not identify:

```text
the runtime binary
the package manager binary
the platform ABI
the dynamic library closure
the environment closure
the filesystem view
the network policy
the entrypoint
the persistent state binding
```

Moreover, dependency output can differ even when the lockfile is unchanged, due to lifecycle scripts, host-bound native builds, package manager version differences, registry behavior, or platform-specific optional dependencies.

Execution Identity therefore separates:

```text
dependency_derivation_hash = how dependencies were produced
dependency_output_hash     = what dependency tree was actually used
```

### 2.6 Source-native distribution needs launch-envelope identity

The cross-platform source distribution problem is not:

```text
Can we ship source code?
```

It is:

```text
Can we distribute source code with enough identity to reconstruct and compare its execution launch conditions?
```

Existing tools identify adjacent objects:

| System              | Identity target             | Strength                                    | Gap                                                      |
| ------------------- | --------------------------- | ------------------------------------------- | -------------------------------------------------------- |
| Reproducible Builds | artifact bytes              | bit-level artifact accountability           | does not identify launches                               |
| Bazel               | hermetic build actions      | host-independent build outputs              | build-centered                                           |
| Nix                 | derivations / store outputs | explicit build inputs and immutable outputs | not a general source-native launch identity              |
| Docker              | images                      | portable filesystem images and isolation    | image digest does not include full `docker run` envelope |
| Package managers    | dependency resolution       | ecosystem-native dependency graph           | runtime/env/fs/policy/entrypoint not included            |
| ReproZip            | traced execution bundle     | post-hoc capture of files/libs/env          | not pre-execution launch identity                        |

This paper proposes Execution Identity as the missing identity object.

---

## 3. Execution Identity

We define **Execution Identity** as a content-addressed representation of a source-native process launch envelope.

```text
execution_id = H(
  source_tree_hash,
  dependency_derivation_hash,
  dependency_output_hash,
  runtime_identity,
  environment_closure,
  filesystem_view_hash,
  network_policy_hash,
  capability_policy_hash,
  entry_point,
  argv,
  working_directory
)
```

The execution identity is computed before launch. It identifies what the process is about to see.

### 3.1 Source identity

Source identity has two parts:

```text
source_ref       = where the source came from
source_tree_hash = what source tree was materialized
```

A Git commit SHA is useful provenance. It identifies a Git commit object. It is not necessarily the same as the source tree that a runtime sees after Git LFS resolution, checkout filters, line-ending normalization, generated files, symlink policy, or platform-specific materialization.

Ato’s hash policy explicitly separates Git commit SHA from Ato-managed source tree hash, payload hash, blob hash, and derivation hash. Git commit SHA is treated as source locator / provenance, not as Ato content integrity. 

This distinction prevents a common category error:

```text
Git commit SHA is provenance.
source_tree_hash is materialized source identity.
```

### 3.2 Dependency identity

Dependency identity is split into derivation and output.

```text
dependency_derivation_hash = H(inputs and policies used to produce dependencies)
dependency_output_hash     = H(materialized dependency tree)
```

A lockfile alone is not sufficient. Dependency output can depend on:

```text
package manager version
runtime version
platform
libc / ABI
package manager config
install command
lifecycle script policy
registry policy
network policy
environment allowlist
system build inputs
```

Ato’s dependency derivation design makes this distinction explicit: `derivation_hash` is install input identity, and `blob_hash` is frozen output identity. It also states that lockfile digest alone is not a valid identity for dependency output. 

This distinction is central for cross-platform execution. The same source and lockfile may produce different dependency outputs on Linux, macOS, Windows, glibc, musl, x86_64, arm64, or GPU-enabled hosts.

### 3.3 Runtime identity

Runtime identity identifies the actual executable runtime.

```text
runtime_identity = {
  declared: "node@20",
  resolved: "node@20.10.0",
  binary_hash: "sha256:...",
  abi: "linux-x64-glibc2.31",
  dynamic_linkage_fingerprint: "...",
  completeness: "binary-with-dynamic-closure"
}
```

The declared version is not enough. `node@20`, `python@3.11`, or `ruby@3.3` may resolve through system PATH, nvm, pyenv, asdf, mise, Volta, Homebrew, package managers, or Ato-managed runtimes. Even if the version string matches, the binary and dynamic library closure may differ.

Runtime identity should therefore include both declared and resolved identity. Where feasible, it should also include dynamic linkage fingerprints. Where not feasible, the identity should record its completeness level:

```text
declared-only
resolved-binary
binary-with-dynamic-closure
best-effort
```

### 3.4 Environment closure

Environment variables are execution inputs. A launch identity must include them explicitly.

```text
environment_closure = {
  env_vars: {
    "PATH": "<managed-runtime-bin>:<managed-tools-bin>",
    "HOME": "<session-home>",
    "LANG": "C.UTF-8",
    "TZ": "UTC"
  },
  fd_layout: {
    stdin: "inherited",
    stdout: "inherited",
    stderr: "inherited"
  },
  umask: "022",
  ulimits: { ... }
}
```

The goal is not merely to record the host environment. The goal is to close and normalize the environment. Host variables should either be explicitly included or explicitly excluded. If an environment variable is allowed to vary, the execution identity should reflect that fact.

This is one reason Docker image identity is insufficient. Docker run-time options can override image defaults, including environment variables.

### 3.5 Filesystem view

A process observes a filesystem view.

```text
filesystem_view_hash = H({
  mounts: [
    { src: "store/blobs/<source>", dst: "/app", mode: "ro" },
    { src: "store/blobs/<deps>",   dst: "/app/node_modules", mode: "ro" },
    { src: "runs/<id>/tmp",        dst: "/tmp", mode: "rw" },
    { src: "state/<app>/data",     dst: "/data", mode: "rw" }
  ],
  case_sensitivity: "...",
  symlink_policy: "...",
  tmp_policy: "session-local"
})
```

This view may include read-only source layers, dependency projections, writable session caches, temporary filesystems, and persistent state bindings. The identity of the view is not equivalent to the identity of any one layer.

A Docker image digest identifies an image. It does not identify bind mounts, volumes, tmpfs, state attachments, or working directory overrides.

### 3.6 Policy identity

Policy affects execution.

```text
policy_identity = H({
  network: {
    mode: "deny-by-default",
    allow: ["api.example.com"]
  },
  capabilities: {
    fs_read: [...],
    fs_write: [...],
    host_bridge: [...]
  },
  sandbox: {
    backend: "landlock+bwrap",
    strength: "strict"
  }
})
```

A process with network access and a process without network access do not have equivalent launch conditions. A process allowed to read host secrets and a process denied access do not have equivalent launch conditions.

Therefore, policy is part of execution identity.

---

## 4. Reproducibility Classes

Cross-platform execution reproducibility is not binary. The correct question is not merely whether an execution is reproducible, but **why** it is or is not reproducible.

We define six reproducibility classes.

```text
reproducibility_class ∈ {
  pure,
  host-bound,
  state-bound,
  time-bound,
  network-bound,
  best-effort
}
```

### 4.1 Pure

A `pure` execution is expected to reproduce from the execution identity alone.

Example:

```text
sealed source tree
sealed dependency output
sealed runtime binary
closed environment
read-only filesystem view
no network
no persistent state
fixed entrypoint and argv
```

### 4.2 Host-bound

A `host-bound` execution depends on host ABI, kernel, driver, CPU feature, GPU runtime, libc, system libraries, or other non-portable host properties.

Example:

```text
native Python extension linked against host libraries
node-gyp module compiled against host libc
GPU workload depending on driver version
```

### 4.3 State-bound

A `state-bound` execution depends on persistent or previous state.

Example:

```text
application database
browser profile
model cache
user workspace
previous generated files
```

State-bound executions may be replayable if the state snapshot is included or referenced.

### 4.4 Time-bound

A `time-bound` execution depends on wall-clock time, monotonic time, timezone, or scheduled behavior.

Example:

```text
date-sensitive tests
license checks
scheduled jobs
```

### 4.5 Network-bound

A `network-bound` execution depends on external network responses.

Example:

```text
live API call
registry lookup
model download
remote feature flag
```

### 4.6 Best-effort

A `best-effort` execution contains uncontrolled or unclassified nondeterminism.

Example:

```text
opaque installer
unclassified lifecycle script
host tool invocation outside sandbox
untracked dynamic dependency
```

The contribution is not the claim that every execution is reproducible. The contribution is making the cause and degree of non-reproducibility explicit.

```text
We do not claim that all executions are reproducible.
We make non-reproducibility identifiable.
```

---

## 5. Ato: A Reference Runtime for Execution Identity

Ato is a source-native execution runtime for local projects, GitHub repositories, and shared application handles. Its README describes it as a command-line tool that detects what a project needs, prepares missing tools, and runs without asking the user to manually install Python, Node, Rust, or project-specific dependencies first. 

Ato is not presented here as a replacement for Nix, Docker, or package managers. It is a reference implementation of Execution Identity for source-native launches.

### 5.1 Layered state

Ato separates session effects, persistent user state, and immutable materialized objects.

```text
~/.ato/
├── runs/
│   └── <session>/
│       ├── workspace/source/
│       ├── workspace/build/
│       ├── deps/
│       ├── cache/
│       └── tmp/
│
├── state/
│   └── <capsule-id>/
│       └── data/
│
└── store/
    ├── blobs/<blob-hash>/
    ├── refs/
    ├── meta/
    └── attestations/
```

The core invariant from Ato’s dependency materialization design is:

```text
store/blobs/<blob-hash>/       immutable payload only
runs/<session>/deps/           dependency projection
runs/<session>/cache/          writable session cache
state/<capsule-id>/data/       writable persistent user state
```



This separation is necessary for launch identity. If dependencies, source tree, persistent state, and session cache are mixed, execution drift cannot be explained.

### 5.2 Source-tree non-pollution

Ato’s Phase A0 is source-tree non-pollution. Dependencies and build outputs should not be written into the user project directory or installed capsule directory. All session-local side effects are confined under `runs/<id>/`.

This phase is deliberately not a full CAS optimization. It is a correctness phase. Before an execution can be reproduced, the system must know which effects belong to the source and which belong to the run.

### 5.3 Dependency materialization

Ato routes dependency installation through a single `DependencyMaterializer`.

Conceptually:

```text
request
  -> create session workspace
  -> compute derivation identity
  -> lookup dependency output blob
  -> install on miss
  -> freeze output
  -> project into run session
```

This produces both dependency derivation identity and dependency output identity.

Ato intentionally begins with whole-tree dependency output caching rather than file-level CAS. Its design rejects file-level CAS as a default because it is complex, makes stack traces and source maps harder to read, worsens debugging workflows, and increases filesystem and inode pressure. 

This is where a careful Nix comparison belongs:

```text
Nix-style store discipline is powerful for package universes.
Ato-style materialization is optimized for exploratory source-native launches.
```

Ato can therefore use Nix store paths and derivation metadata as components of dependency output identity, while extending the identity boundary outward to the launch envelope: runtime selection, dependency projection, environment closure, filesystem view, network policy, capability policy, entrypoint, arguments, working directory, and state bindings.

### 5.4 Hash-domain separation

Ato separates hash domains.

```text
Git commit SHA      = source locator / provenance
source_tree_hash    = materialized source identity
derivation_hash     = dependency input identity
payload_hash        = artifact integrity
blob_hash           = immutable store object identity
```

The hash policy explicitly states that these domains should not be collapsed into one hash. 

Execution Identity generalizes this principle to the full launch envelope.

### 5.5 Run and install

Ato distinguishes `run` and `install`.

```text
ato run     = ephemeral session + reusable verified materialization
ato install = persistent app identity + state + permissions + refs
```

This distinction matters because installing introduces persistent identity and state binding. A run can be `pure` or `best-effort`; an install often becomes `state-bound`.

---

## 6. Replay and Drift

### 6.1 Replay as launch-envelope reconstruction

Execution Identity enables replay, but replay must be defined carefully.

```text
replay(execution_id) = reconstruct the pre-execution launch envelope
```

This is not deterministic trace replay. It does not guarantee identical instruction traces, syscall ordering, timing, scheduler behavior, or network responses.

ReproZip traces a command after execution using operating-system calls and identifies binaries, files, libraries, dependencies, and environment variables needed for future re-execution.  Execution Identity differs because it identifies the launch envelope before execution.

A replay command might look like:

```text
ato replay <execution_id>
```

It would:

1. Resolve the execution receipt.
2. Fetch or verify source and dependency blobs.
3. Resolve runtime identity.
4. Reconstruct environment closure.
5. Reconstruct filesystem view.
6. Apply network and capability policy.
7. Launch the same entrypoint, arguments, and working directory.

### 6.2 Drift

Drift is the production of different execution identities from apparently similar user intent.

```text
drift = same source_ref, different execution_id
```

Examples:

```text
same Git branch, different commit
same commit, different LFS materialization
same source tree, different dependency output
same lockfile, different package-manager version
same runtime version, different binary hash
same dependency tree, different environment closure
same source and deps, different filesystem mount
same app, different persistent state binding
```

This reframes “works on my machine” as an identity problem. The run did not mysteriously change. The launch envelope changed.

---

## 7. Evaluation

### 7.1 Execution identity stability

Run the same source project repeatedly under controlled and perturbed conditions.

Conditions:

```text
same host, same day
same host, different day
same OS, different machine
different OS
different runtime manager
different environment variables
different timezone
different mount layout
different state binding
```

Metrics:

```text
execution_id stability
component-level diff
classification assigned
false stability rate
false drift rate
```

### 7.2 Drift detection

Intentionally change one launch component at a time.

| Perturbation                          | Expected changed component         |
| ------------------------------------- | ---------------------------------- |
| Replace Node binary                   | `runtime_identity`                 |
| Change `PATH`                         | `environment_closure`              |
| Add bind mount                        | `filesystem_view_hash`             |
| Change timezone                       | `environment_closure`              |
| Same lockfile, different pnpm version | `dependency_derivation_hash`       |
| Lifecycle script downloads binary     | `dependency_output_hash` and class |
| Add persistent database binding       | `filesystem_view_hash` and class   |

### 7.3 Replay success by class

Measure replay as launch-envelope reconstruction.

| Class           | Expected replay behavior                     |
| --------------- | -------------------------------------------- |
| `pure`          | high launch and behavior consistency         |
| `host-bound`    | high on same host, lower cross-host          |
| `state-bound`   | high with state snapshot                     |
| `time-bound`    | high if clock is pinned                      |
| `network-bound` | launch replay possible, behavior may diverge |
| `best-effort`   | diagnostic only                              |

### 7.4 Source-tree non-pollution

For a corpus of Node, Python, Rust, and mixed-language projects, compare the source tree before and after execution.

Metrics:

```text
new files in source tree
modified files in source tree
deleted files in source tree
untracked dependency directories
untracked build outputs
```

Expected result: Ato’s A0 prevents dependency and build-output pollution.

### 7.5 Cross-platform execution drift

Run the same source reference across Linux, macOS, and Windows.

Measure:

```text
which components differ
whether differences are expected
whether differences are explained by reproducibility class
whether an equivalent launch envelope can be reconstructed
```

This evaluation directly addresses the paper’s central problem: cross-platform source-native distribution.

---

## 8. Related Work

### 8.1 Reproducible Builds

Reproducible Builds defines artifact reproducibility as bit-by-bit identical outputs from the same source, build environment, and build instructions.  Execution Identity addresses launch reproducibility rather than artifact reproducibility.

### 8.2 Bazel and hermetic builds

Bazel’s hermeticity isolates builds from host changes and uses specific tool and dependency versions to produce stable outputs.  Execution Identity applies similar explicitness to runtime launch conditions.

### 8.3 Nix

Nix derivations describe build tasks with explicit inputs, system type, builder, arguments, environment variables, and outputs.  This is the closest philosophical foundation, but the identity object differs. Nix identifies derivations and store outputs; Execution Identity identifies source-native launches.

### 8.4 Docker and OCI

Docker provides portable image-based execution. However, `docker run` combines image identity with options, command, arguments, environment variables, mounts, working directory, user, and networking.  Execution Identity identifies this complete launch envelope.

### 8.5 ReproZip

ReproZip traces executed commands and packages binaries, files, libraries, dependencies, and environment variables for future re-execution.  It is post-hoc capture. Execution Identity is pre-execution identity.

### 8.6 Package managers and lockfiles

Package managers identify dependency graphs and resolutions within ecosystems. They are necessary but do not identify runtime, environment, filesystem view, policy, entrypoint, or state.

---

## 9. Discussion

### 9.1 “Same execution result” is too strong

This paper does not claim that the same source code will produce the same behavior on every platform. That claim is too strong. Different kernels, filesystems, clocks, drivers, GPU stacks, external services, and random sources can affect behavior.

The claim is narrower:

```text
The same execution_id identifies equivalent launch conditions.
```

Reproducibility classes then explain how much behavioral reproducibility to expect.

### 9.2 Why not require Nix?

Nix is powerful, but requiring every source-native project to become a Nix package changes the user workflow. Many users and agents want to run a repository before packaging it. Execution Identity addresses this earlier phase.

Ato can borrow from Nix without adopting the full package-universe model:

```text
Borrow explicit input identity.
Borrow immutable store objects.
Borrow closure thinking.
Do not require source-native execution to become package authoring.
```

### 9.3 Why not use Docker images?

Docker images are excellent distribution artifacts. But the launch is not the image. The launch is image plus options, mounts, environment, command, arguments, policy, and state. Execution Identity targets the launch.

### 9.4 Why not deterministic record/replay?

Record/replay systems capture execution traces. That is stronger than launch identity but narrower and heavier. Execution Identity is useful even when deterministic replay is impossible. It explains drift before execution begins.

### 9.5 Security and privacy

Execution receipts may contain sensitive data: paths, environment variable names, state bindings, policy rules, and dependency information. Secret values should not be recorded directly. Receipts should classify and redact sensitive fields.

---

## 10. Conclusion

Cross-platform source-native software distribution needs more than reproducible builds, package locks, container image digests, or post-hoc execution traces.

Reproducible builds identify artifacts. Nix identifies derivations and store outputs. Docker identifies images. Package managers identify dependency resolutions. ReproZip captures observed execution dependencies after a run.

Execution Identity identifies launches.

It makes source-native execution accountable by hashing source tree identity, dependency derivation and output identity, runtime identity, environment closure, filesystem view, network and capability policy, entrypoint, arguments, and working directory. Reproducibility classes then explain whether the launch is pure, host-bound, state-bound, time-bound, network-bound, or best-effort.

Ato implements this model as a source-native runtime. It separates session-local effects, persistent user state, immutable store objects, mutable refs, and execution metadata. Its goal is not to replace Nix, Docker, or package managers, but to define the missing identity layer for cross-platform source execution.

The central claim is:

```text
Nix makes build inputs explicit.
Docker makes filesystem images portable.
Execution Identity makes launches accountable.
```

---

# Appendix A: Revised one-paragraph pitch

Reproducible builds made artifacts accountable, but cross-platform source distribution requires making launches accountable. The same source tree can execute differently depending on runtime resolution, dependency materialization, dynamic libraries, environment variables, filesystem mounts, network policy, persistent state, locale, timezone, entrypoint, arguments, and working directory. Existing systems identify adjacent objects: Nix identifies derivations and store outputs, Docker identifies images, package managers identify dependency resolutions, and ReproZip captures traces after execution. We propose Execution Identity, a content-addressed representation of the pre-execution launch envelope. An execution identity allows a system to compare, replay, and explain source-native executions across platforms without claiming impossible deterministic behavior for all programs.

---

# Appendix B: 強い一文候補

```text
Reproducible builds answer: did we produce the same artifact?
Execution Identity answers: did we launch the same world?
```

```text
Nix makes build inputs explicit.
Docker makes filesystem images portable.
Execution Identity makes launches accountable.
```

```text
The launch, not the image, is the unit of execution reproducibility.
```

```text
A Git commit identifies where the source came from.
A lockfile identifies dependency intent.
An image digest identifies a filesystem artifact.
An execution identity identifies the world a process was launched into.
```

```text
Works on my machine is not a mystery; it is an unaccounted launch envelope.
```
