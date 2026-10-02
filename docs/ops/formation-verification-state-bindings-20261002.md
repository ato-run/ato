# Explicit verification state binding — 2026-10-02

Status: partial implementation, not deployed. Base Ato `9ed8ea05`
(application code `1de0e3a0`), API `fa825a17`. No authority/schema/migration change.
Draft design: [ADR-038](../rfcs/draft/ADR-038-formation-verification-state-bindings.md).

Runtime-only binding joins the existing assigned launch context, logical state
attachment and resolved working copy. An explicit slot mapping supports the
different Derivation slot `app.data` and existing control-plane key `app_data`.
The common process launcher and retained realizer preserve revision/fence/access/
mount; candidate cleanup removes scratch while ownership of attached state stays
with the trusted caller. No new state store, host-path wire field, Capsule identity,
source rewrite or app-specific migration command is introduced.

Before use, the caller must authenticate owner, assigned Run/lease and current
writer fence through existing control-plane services. This library cannot mint
that authority. Source exploration and the production Worker still supply no
borrowed state. Persistence is not advertised as available on that unbound path.
This implementation alone does not complete Kutt or Coordinator binding acceptance.

Local validation: Runtime library 85 PASS, Worker library 75 PASS, followed by
one additional actual launcher rejection test PASS (86 distinct Runtime cases).
Eight state-boundary tests cover exact logical/private resolution, read/write
fences, missing/duplicate/undeclared slots, mount/access mismatch, symlink and
source/cleanup overlap. The launcher test rejects overlap/unscoped bindings before
it takes scratch ownership and proves the existing state sentinel remains.
Formatting and CLI/Worker/Runtime all-target clippy `-D warnings` PASS.

Existing API state tests at `fa825a17`: 39 PASS / 1 FAIL. The failing recovery test
reads `runs.run_h3` without having inserted it. Same-condition merged-main
`9e003190` also reports 39 PASS / 1 identical FAIL. This is a baseline test
failure, not a new state-binding failure; do not count it as passing or suppress it.

Actual Linux Runtime code `e185f2d97d869615e23a83f950efb147d317f6b7`,
unchanged authority source `3ac36fa2`, exact Cargo lock: changedetection.io retained
artifact from trial 14 launched through the common retained realizer and process
executor with explicitly bound `/data`. All three fresh frozen-K receipts PASS,
all three confirmed stop/cleanup, zero allowed Runtime network bytes. Chromium
149 created/edited/paused an owned test watch, then a fresh process and browser
context found the same title and paused state. Deletion and final cleanup PASS.
Private working copy survived candidate cleanup; it was not captured into D or
the retained source artifact. External fetch remains unverified.

This is a local binding fixture. Its assignment and fence values do not claim
Coordinator authentication. The first helper received stdin EOF and stopped
before UI interaction; preserve its fresh receipt/cleanup. Controlled runs 2/3
performed the UI measurement. Runs 1/2 used separate journals with a repeated
fixture attempt label; helper v2 gives run 3 a unique actual attempt ID
`functional-state-owned-verification-fixture-3`. Preserve both helper versions
and preregistrations. No original Search deadline/budget was resumed or reset.
Proof SHA-256 `f925202933e2e2ea25b4f00e118e5232650c4cc891a2b24abb63ad462790e5ac`.
Linux CLI/Worker and locked/offline helper builds PASS. The initial unpinned
external helper check is preserved; the executed helper dependency versions
match the repository lock, with only its own root package added.

Required next evidence: control-plane
current-owner/current-assignment and stale-writer refusal, initialized-state input
for Kutt, same frozen K, migration/JWT, representative UI and retained state.
The bindings neither authorize an ordinary Run nor publish/deploy the successful D.
Zero paid API calls; old UNKNOWN/WBO and old source acceptance remain untouched.
