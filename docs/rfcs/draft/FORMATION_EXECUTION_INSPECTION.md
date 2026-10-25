# Formation execution inspection

Status: draft implementation contract, 2026-10-23.

## Decision

Extend Formation's evidence and inspection boundary without adding a second
execution plan, semantic root, provider-bearing identity, or database table.
`requirements::inspection::inspect(D, K)` borrows the existing requirements,
inputs, runtimes, step order, Ports, state, variable requirements and K.
Components are existing BoundSteps; CPU limits remain adapter runtime facts,
memory/VRAM/scratch remain HostRequirement, external destinations remain
phase-scoped NetworkRequirement. The view never claims measured readiness.
Provider, price, physical machine and lease remain attempt evidence.

`repository_evidence::inspect` and `ato form-inspect <directory>` provide
bounded source evidence, independently of the legacy detector and compiler.
Python/Node/Rust manifest declarations, Docker exposed-port declarations and
volume markers, Compose presence, GUI/TUI/Web/GPU/model dependency hints,
entrypoint names and example credential names are recorded without executing
the repository. README text is untrusted: only fixed enum markers leave the
scanner. Scripts, credential values, env values and raw source never leave it.
`--derivation D.json --contract K.json` optionally adds the borrowed declaration
view. Both files are required together and individually bounded to 1 MiB; the
inspection neither admits nor executes them.

Each selected file is at most 64 KiB; over-limit, malformed structured, linked,
non-regular and non-UTF8 files are listed as unscanned. No partial parse is
promoted into evidence. Output sets are sorted and bounded. Source roots must
be authorized directories; production callers inspect frozen source closures.
This scanner does not itself freeze a mutable checkout and does not claim to
provide a race-proof filesystem sandbox. Its result has no execution authority.

## Evidence, authoring, verification

A manifest declaration records what the repository declares. A dependency or
README marker is a hint: torch does not prove CUDA is required; a GUI library
does not prove the application has a GUI; Compose presence does not resolve its
service graph. These remain separate from authored/compiled D and observed
Runtime evidence. No inferred minimum RAM, GPU SKU, public Port, state mount,
secret value, network grant, retry or execution command is fabricated.

Existing Preset and CandidateProducer → ProposalValidator → compiler paths
remain the only routes from source evidence to authorized D. Frozen K remains
owned by the requester/compiler, and only existing Rust verification can
produce a satisfied receipt. `runtime_verified=false` is explicit on both
inspection views. Surface ready and K satisfaction remain independent.

## Compatibility and scope

No field is added to BoundDerivation, BoundContract, wire v2/v3/v4, existing
DetectorEvidence or the old detector. Existing canonical bytes remain intact.
The borrowed view is inspection output only and has no Deserialize/admission
entry. Existing code does not deserialize the new source inspection as a plan.

This slice does not resolve Compose interpolation/includes/extends or dependency
DAGs, infer launch argv, expose native surfaces, provision GPUs, issue managed
secrets, change fork semantics, or deploy a new runtime. These are separate
implementation gates in the investigation ledger. New capabilities must pass
the existing authority, budget, execution, cleanup and verification boundaries.

## Validation

Fixtures cover Python GPU/model/GUI/Web, Node and Rust TUI, Docker and Compose,
malformed/oversized/binary sources, source symlinks, deterministic empty reports,
credential value exclusion, README injection and non-execution of scripts.
The D/K inspection regression verifies borrowed canonical objects, omitted
environment values and unchanged ContractRef/DerivationRef.

## Automatic requester integration

The source-authorized v2 Proposal request now includes an optional
`repository_evidence` projection from the requester's frozen source directory.
The bounded scanner reads only the fixed root manifest/document allowlist. It
does not execute scripts, read `.env`, follow file symlinks or send source text,
commands, credential values, host paths or endpoints in this projection.

The evidence validator rejects unknown filenames, arbitrary name text, duplicate
or oversized collections, invalid Ports and `runtime_verified=true`. Provider
input remains advisory. Existing frozen K, explicit runtime constraint, operation
catalog, proposal validation, execution requirement ceiling, durable attempt
ledger and search budgets retain all authority. Neither a GPU/GUI hint nor a
Compose declaration authorizes additional resources, keys, egress or execution.

Requests created without this optional projection keep their previous bytes.
Source text must already be authorized for this v2 provider request. Runtime
verification remains mandatory and a source declaration is never promoted to a
successful Formation or receipt. Unsupported observations/routes fail through
the existing admission path; this change does not claim arbitrary GUI/GPU plans
can already be generated and executed.
