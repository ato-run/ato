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
reads `runs.run_h3` without having inserted it; same-condition merged-main base
comparison is pending. Do not count that failure as passing or suppress it.

Required next evidence: actual contained stop/restart state survival, control-plane
current-owner/current-assignment and stale-writer refusal, initialized-state input
for Kutt, same frozen K, migration/JWT, representative UI and retained state.
The bindings neither authorize an ordinary Run nor publish/deploy the successful D.
Zero paid API calls; old UNKNOWN/WBO and old source acceptance remain untouched.
