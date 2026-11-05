# Startup conditions and capability admission

Status: Draft

A portable route can declare a bounded startup observation on one of its
logical HTTP Ports. The observation is a readiness condition, not permission
to bind arbitrary Ports or contact external hosts. The attempt must still
realize that declared Port, wait for the condition, and verify the unchanged
Contract against actual Run evidence. Startup timeout, failure or cancellation
must not produce a successful receipt.

The attempt admission boundary currently treats every nonempty
`ExecutionRequirements` as requiring a capability grant. As a result, a
disposable route with only a startup condition is refused by ordinary Run
admission even though the requirements' existing `within` method explicitly
excludes startup from grant comparison. This prevents authored applications
with an explicit readiness budget from running through the ordinary path.

For ordinary admission, require a separate grant when the route requests
network or authority capabilities. A startup condition by itself does not
grant either. Host constraints remain independently fail-closed until measured
by a capable Runner. The existing effect-class restrictions, derivation-bound
authorization, startup validation and actual startup probe remain in force.
Formation and verification paths with a scoped ceiling retain their current
subset checks; this proposal does not extend their execution capabilities.

Tests must cover a startup-only route, startup combined with network or
authority requests, a mismatched user-invoked DerivationRef, unsupported host
constraints, and startup failure without a successful verification receipt.
The same implementation must serve local and hosted attempts, without
rewriting the Contract or silently removing the declared startup condition.
