# ADR-018 — The substrate a Formation build runs in

**Status**: proposed
**Context**: B1.3, and the gate on whether untrusted public sources may be built at all

## The question

A Formation build executes code chosen by whoever submitted the repository:
`uv sync` runs arbitrary `build_backend` hooks, a `setup.py` runs at install
time, and a postinstall script is ordinary practice. The substrate has to be
chosen on the assumption that the build IS the attacker, not that it might
misbehave.

That is a different threat model from P3's runtime sandbox, and the two must
not be collapsed into one policy. A runtime workload was already accepted by an
owner who chose to run it; a Formation build has not been accepted by anyone
yet, and its output is what everyone downstream will trust.

## Candidates, measured against what this tree actually has

### 1. Bubblewrap + Landlock (`ato-sandbox`, extracted in P3.6)

Present and proven on `ubuntu-sugamo`: bwrap 0.11.1, kernel 7.0, and the P3
acceptance measured a host sentinel unreadable, `/app` read-only, arbitrary host
writes landing in an ephemeral root and leaving nothing behind.

- **For**: already here, already exercised, cheap to start, no daemon
- **Against**: shares the host kernel. A kernel-level escape is a host
  compromise, and the build is untrusted code by assumption. Network policy is
  all-or-nothing at the namespace level — `--unshare-net` is genuine isolation,
  but a build that needs PyPI needs the network, and bwrap alone gives no way
  to allow PyPI and deny everything else.

### 2. Disposable Firecracker microVM

The tree has a Firecracker substrate (`extensions/materializers/snapshot`,
`services/netd`, TAP networking, the hosted runners on staging run it).

- **For**: a hardware-virtualization boundary, which is the right strength for
  untrusted code. Network can be mediated at the TAP interface, so "PyPI only"
  is expressible. A VM is disposable by construction.
- **Against**: the existing Firecracker path is built around *snapshot
  capture and restore*, not around "run a build and collect an output tree".
  Reusing it means a new guest lifecycle, not a new caller of the old one. It
  needs KVM, which the Hetzner cloud runner does not have (recorded in
  `hetzner-dedicated-runner-setup`).

### 3. Rootless container / rootless BuildKit

- **For**: the standard answer, good caching, ecosystem support.
- **Against**: nothing in this tree runs it today, so it is a new dependency
  and a new operational surface. Rootless still shares the kernel, so it does
  not buy the boundary Firecracker does — it mostly buys ergonomics.

### 4. Privileged Docker / the host Docker socket

Explicitly refused. Handing an untrusted build the Docker socket is handing it
the host.

## Decision

**Two lanes, gated separately, and the gate is stated rather than implied.**

**Lane 1 — no-network builds run under Bubblewrap + Landlock, now.**
A Static Web build takes a source tree and produces files. With
`--unshare-net`, the build cannot reach anything, and the isolation P3 already
measured is the isolation this needs. This lane may be enabled for untrusted
public sources.

**Lane 2 — dependency-resolving builds (Python) start trusted-only.**
`uv sync` needs the network, and bwrap cannot express "PyPI and nothing else".
Running untrusted code with unrestricted host network access while calling it
sandboxed would be the specific dishonesty this ADR exists to prevent. So the
Python lane is enabled for an **allowlist** on staging, is **not** enabled for
public publish, and its provenance records the exact network policy that was in
force.

**Lane 2 becomes public when — and only when — one of these lands:**

- a mediated egress path (a proxy the build must go through, allowlisted to the
  package index), so the network policy is enforceable rather than aspirational;
  **or**
- a disposable Firecracker build VM with TAP-level egress control, on a host
  with KVM.

Until then the honest report is "Python Formation works, and is not open to
untrusted sources", not "Python Formation is sandboxed".

## What this ADR refuses to do

- report an unrestricted host network as isolation
- enable untrusted public Python publish on the strength of a filesystem
  sandbox alone
- treat P3's runtime policy as sufficient for Formation, or the reverse
- fall back to running unconfined when the sandbox is unavailable: a Formation
  worker that cannot contain a build must refuse the job

## Consequences

B1's Python acceptance runs on the allowlist lane, and B1 completes with public
untrusted Python publish **explicitly not enabled**. That is a stated boundary,
not a gap discovered later.
