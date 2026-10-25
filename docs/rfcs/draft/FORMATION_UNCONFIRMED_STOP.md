# Formation candidate termination evidence

Status: draft implementation contract; extends the existing internal Runtime
Network v0 and ADR-028 UNKNOWN handling. No deployment or remote migration.

A durable finished attempt record establishes the recorded execution history.
It does not establish that every owned workload has terminated. A fresh K
verification receipt remains evidence of its observation point even if the
subsequent physical stop is uncertain. Unconfirmed termination permits neither
automatic fallback nor publication or writer reassignment.

The Runtime attestation gains optional `candidate_stop`: `confirmed`,
`scratch_kept` (terminated, scratch removal failed), or `unconfirmed`.
Absent historical fields retain their existing interpretation. A present field
requires execution to have started. This field carries no log, host resource
path, variable value, or recovery authority; original private ownership records
remain the recovery evidence. It does not enter K, D, or Capsule identity.

The existing common `StopClass` supplies this field for source and retained
attempts. A consumed process handle after failed termination is not absence of
execution: temporary realization remembers the failure through teardown and
Drop, keeps its scratch and reports typed unconfirmed termination. Scratch
removal failure after confirmed termination is separately typed `ScratchKept`.
Worker-level cleanup also retains the entire owned attempt tree if termination
or execution history is uncertain. Confirmed cleanup still removes only owned
attempt scratch, never borrowed state.

The Coordinator records `unknown` with reason `candidate_stop_unconfirmed`
under the existing attempt fence and atomic search-hold triggers, irrespective
of PASS/FAIL or a finished journal. It preserves the result and K receipt as
evidence, creates no VerifiedRoute, and blocks further candidates/proposals
within that Search. Publication refuses unconfirmed stop. Existing owner
resolution requires explicit execution-stop evidence and does not alter or
rerun historical attempts. No timeout, retry, receiver restart, or stronger
effect classification clears this hold. Budgets and deadlines remain frozen;
UNKNOWN reservations retain their existing conservative accounting.

API support must precede a Worker emitting this optional field. No automatic
deployment/feature enabling is authorized by this document. Receipt authority
and canonical compiler are unchanged; confirmed-stop reports must pass their
existing fresh same-K checks. Physical-stop fault tests, real Coordinator
storage and owned contained workload tests are recorded separately from
autonomous application/provider acceptance. Old WBO UNKNOWN is preserved.
