# Compatibility and Derivation transformation

Status: Current technical reference.

Compatibility means preserving Contract K and producing a verified new
realization. It does not require keeping the old implementation forever.
K states the observable obligations being preserved; the strength of the
compatibility claim is bounded by those obligations.

**Derivation Transformation** replaces D with a candidate D under the frozen
canonical K. Source or runtime inputs may change when K allows them to change.
The existing Rust verifier must verify the candidate's observations against K.
If K captures source identity, changing the source cannot preserve that K by
silently weakening or recapturing its requirement.

The current CLI `ato reform` writes a local candidate, receipt and report. It
does not publish, activate or switch an Instance. See the
[transformation guide](../guides/derivation-transformation.md).

**Continuous Patching** combines candidate generation, dependency/advisory
evidence, rebuild, K verification, state/effect assessment and controlled
rollout. The repository contains mechanisms and bounded experiments for parts
of this process. That does not establish automatic production rollout, a
general advisory engine, or universal state migration. The historical
[patching ledger](../archive/2026/ops/continuous-patching-ledger.md) is dated
evidence; the [runtime image ledger](../operations/runtime-image-ledger.md)
records declared artifacts rather than proving an image was scanned.

Even when `K_old = K_new` and a new receipt is fully satisfied, independently
assess state continuity, side effects, security, supported platforms and
rollback. K equality alone guarantees none of those properties outside K.
The [stateful transformation experiment](../archive/2026/ops/stateful-transformation-v1.md)
uses fresh process memory; it explicitly does not prove restart persistence.
