//! Deterministic integer floor-first capped equal-share CPU entitlement.
//!
//! The runner serves several restored snapshots at once, each pinned to a fixed
//! guest Machine Shape (2 vCPU / 3 GiB — see ADR-016). This module does NOT
//! touch that shape: a snapshot's `vcpu_count` is baked into the checkpoint and
//! cannot change at restore. What it decides is the HOST-side `cpu.max` quota
//! each running session is entitled to out of a shared millicore budget — a
//! knob entirely outside snapshot identity.
//!
//! The policy: every active session is guaranteed its minimum first, then the
//! leftover budget is filled evenly up to each session's maximum, and any
//! session that saturates below the even share releases the difference to the
//! others. It is intentionally NOT weighted — v1 has no priority classes beyond
//! the min/max a request already carries.
//!
//! v1 additionally requires every request to share ONE floor (`min_millis`).
//! Both performance classes — economy (1000/1000) and standard (1000/2000) —
//! use a 1000m floor, so this holds by construction, and under a shared floor
//! "floor-first capped equal-share" IS textbook max-min fairness. Mixed floors
//! are rejected (`NonUniformMinimum`) rather than silently producing a result
//! that only *looks* max-min-fair; lifting that restriction is a later slice if
//! a class with a different floor is ever introduced.
//!
//! Everything here is pure integer arithmetic over millicores (1000m = 1 CPU):
//! no floats (a float rounding difference across hosts would make two runners
//! disagree on the same inputs), no clock, no I/O. The cgroup writer and the
//! single-owner manager actor that call it live in later slices; this module is
//! the math and nothing else, so it can be unit-tested exhaustively and carries
//! zero production behavior on its own.

// PR 1 lands the pure allocator ahead of its caller: the CpuEntitlementManager
// actor and cgroup writer that consume these items arrive in PR 2. Until then
// the public surface is exercised only by this module's own tests, so the
// unused-code lints are expected — not dead code, just not-yet-wired code.
#![allow(dead_code)]

use std::collections::BTreeMap;

/// One session's CPU entitlement request, as the API resolved it.
///
/// `min`/`max` are millicores and come from the server-confirmed performance
/// class, never from raw client input. `slot_index` is the session's execution
/// slot — the tie-break that makes leftover distribution deterministic and
/// independent of the order requests happen to arrive in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuRequest {
    /// Lease id — the allocation map key. Opaque here.
    pub lease_id: String,
    /// Execution slot index owning this session. Unique per active session and
    /// used as the deterministic ordering key.
    pub slot_index: usize,
    /// Guaranteed floor, millicores. Must be > 0 and <= `max_millis`.
    pub min_millis: u32,
    /// Ceiling, millicores. Never exceeded regardless of spare budget.
    pub max_millis: u32,
}

/// Why an allocation could not be produced.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CpuAllocationError {
    /// The sum of every active request's minimum exceeds the runner budget, so
    /// admitting them all would violate at least one floor. The caller (claim
    /// path) rejects the NEW request rather than shrinking a running one below
    /// its guarantee.
    #[error(
        "minimum CPU guarantees ({requested_min_millis}m) exceed the runner budget \
         ({budget_millis}m); cannot admit without breaking a floor"
    )]
    InsufficientMinimumCapacity {
        budget_millis: u32,
        requested_min_millis: u32,
    },
    /// A request is malformed (min == 0, or min > max). Rejected before it can
    /// poison the shared allocation.
    #[error("invalid CPU request for lease {lease_id}: {reason}")]
    InvalidRequest { lease_id: String, reason: String },
    /// Two requests carry the same `lease_id`. The allocation is a map keyed by
    /// lease id, so a duplicate would silently overwrite an entry and let two
    /// requests share one slot's accounting — floors, caps and the budget sum
    /// would all lose meaning. Rejected fail-closed.
    #[error("duplicate lease_id {lease_id} in CPU request set")]
    DuplicateLeaseId { lease_id: String },
    /// v1 requires every active request to share one floor (see module docs):
    /// the allocation policy is floor-first capped equal-share, which only
    /// coincides with true max-min fairness when all minimums are equal. Mixed
    /// floors are rejected rather than silently mis-shared.
    #[error(
        "non-uniform CPU minimums ({first_min_millis}m vs {other_min_millis}m for \
         lease {other_lease_id}); v1 requires a single shared floor"
    )]
    NonUniformMinimum {
        first_min_millis: u32,
        other_lease_id: String,
        other_min_millis: u32,
    },
}

/// The resolved per-lease entitlement, keyed by `lease_id`, in millicores.
///
/// Invariants the allocator guarantees (asserted in tests):
///   * every lease gets at least its `min_millis`;
///   * no lease exceeds its `max_millis`;
///   * the sum never exceeds `budget_millis`;
///   * the result is a pure function of the (budget, request-set) — the input
///     ordering does not matter, only each request's `slot_index`.
pub type CpuAllocation = BTreeMap<String, u32>;

/// Allocate `budget_millis` across `requests` by deterministic integer
/// floor-first capped equal-share (= max-min fairness under the v1 shared-floor
/// invariant). See the module docs for the policy.
///
/// `requests` may be given in any order; the outcome depends only on each
/// request's `slot_index`. An empty request set yields an empty allocation.
pub fn allocate_cpu(
    budget_millis: u32,
    requests: &[CpuRequest],
) -> Result<CpuAllocation, CpuAllocationError> {
    // Validate every request up front — a single malformed entry fails the
    // whole allocation rather than being silently coerced.
    for request in requests {
        if request.min_millis == 0 {
            return Err(CpuAllocationError::InvalidRequest {
                lease_id: request.lease_id.clone(),
                reason: "min_millis must be positive".to_string(),
            });
        }
        if request.min_millis > request.max_millis {
            return Err(CpuAllocationError::InvalidRequest {
                lease_id: request.lease_id.clone(),
                reason: format!(
                    "min_millis ({}) exceeds max_millis ({})",
                    request.min_millis, request.max_millis
                ),
            });
        }
    }

    // Reject duplicate lease ids: the allocation is keyed by lease_id, so a
    // duplicate would collapse two requests onto one map entry and corrupt every
    // floor/cap/budget guarantee.
    let mut seen_lease_ids = std::collections::BTreeSet::new();
    for request in requests {
        if !seen_lease_ids.insert(request.lease_id.as_str()) {
            return Err(CpuAllocationError::DuplicateLeaseId {
                lease_id: request.lease_id.clone(),
            });
        }
    }

    // v1 shared-floor invariant: a single common minimum is what makes
    // floor-first capped equal-share equal to max-min fairness.
    if let Some(first) = requests.first() {
        for request in &requests[1..] {
            if request.min_millis != first.min_millis {
                return Err(CpuAllocationError::NonUniformMinimum {
                    first_min_millis: first.min_millis,
                    other_lease_id: request.lease_id.clone(),
                    other_min_millis: request.min_millis,
                });
            }
        }
    }

    // Work over a slot-ordered copy so leftover distribution is deterministic
    // and a duplicate slot_index (a bug upstream) is caught rather than making
    // the fill order ambiguous.
    let mut ordered: Vec<&CpuRequest> = requests.iter().collect();
    ordered.sort_by_key(|request| request.slot_index);
    for pair in ordered.windows(2) {
        if pair[0].slot_index == pair[1].slot_index {
            return Err(CpuAllocationError::InvalidRequest {
                lease_id: pair[1].lease_id.clone(),
                reason: format!("duplicate slot_index {}", pair[1].slot_index),
            });
        }
    }

    // Sum floors in u64 so a pathological request set (many slots × large min)
    // cannot overflow before the budget check; budget is bounded to u32 so the
    // comparison and the subtraction below are exact once we know min fits.
    let min_total: u64 = ordered
        .iter()
        .map(|request| u64::from(request.min_millis))
        .sum();
    if min_total > u64::from(budget_millis) {
        return Err(CpuAllocationError::InsufficientMinimumCapacity {
            budget_millis,
            // Saturating cast for the diagnostic only; the comparison above used
            // the exact u64 value.
            requested_min_millis: min_total.min(u64::from(u32::MAX)) as u32,
        });
    }
    // Safe: min_total <= budget_millis (u32), so it fits u32.
    let min_total = min_total as u32;

    // Everyone starts at their floor; `remaining` is the spare budget to fill.
    let mut allocation: CpuAllocation = ordered
        .iter()
        .map(|request| (request.lease_id.clone(), request.min_millis))
        .collect();
    let mut remaining = budget_millis - min_total;

    // Water-fill: each pass hands an equal integer share to every not-yet-capped
    // request. A request that caps below the share returns the difference, so
    // the next pass recomputes a larger share over the smaller unsaturated set.
    // When the even share rounds down to zero, a final single-millicore pass
    // distributes the residue in slot order.
    while remaining > 0 {
        let unsaturated: Vec<&CpuRequest> = ordered
            .iter()
            .copied()
            .filter(|request| allocation[&request.lease_id] < request.max_millis)
            .collect();
        if unsaturated.is_empty() {
            break;
        }
        let share = remaining / unsaturated.len() as u32;
        if share == 0 {
            // Residue smaller than the unsaturated count: give 1m each, in slot
            // order, to exactly `remaining` requests. This consumes the residue
            // in one pass (each recipient still has >=1m headroom, being
            // unsaturated), so we are done.
            for request in unsaturated {
                if remaining == 0 {
                    break;
                }
                *allocation.get_mut(&request.lease_id).unwrap() += 1;
                remaining -= 1;
            }
            break;
        }
        let mut granted_this_pass = 0u32;
        for request in unsaturated {
            let current = allocation[&request.lease_id];
            let grant = share.min(request.max_millis - current);
            *allocation.get_mut(&request.lease_id).unwrap() += grant;
            granted_this_pass += grant;
        }
        // No forward progress despite a positive share means everyone is capped;
        // stop rather than spin.
        if granted_this_pass == 0 {
            break;
        }
        remaining -= granted_this_pass;
    }

    Ok(allocation)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(lease: &str, slot: usize, min: u32, max: u32) -> CpuRequest {
        CpuRequest {
            lease_id: lease.to_string(),
            slot_index: slot,
            min_millis: min,
            max_millis: max,
        }
    }

    /// standard = min 1000 / max 2000; economy = min 1000 / max 1000.
    const STD_MIN: u32 = 1000;
    const STD_MAX: u32 = 2000;
    const ECO: u32 = 1000;

    fn sum(alloc: &CpuAllocation) -> u32 {
        alloc.values().copied().sum()
    }

    #[test]
    fn single_standard_takes_its_max() {
        let alloc = allocate_cpu(8000, &[request("a", 0, STD_MIN, STD_MAX)]).expect("allocates");
        assert_eq!(alloc["a"], 2000);
    }

    #[test]
    fn single_economy_stays_at_floor() {
        let alloc = allocate_cpu(8000, &[request("a", 0, ECO, ECO)]).expect("allocates");
        assert_eq!(alloc["a"], 1000);
    }

    #[test]
    fn four_standard_with_ample_budget_all_reach_max() {
        let reqs: Vec<_> = (0..4)
            .map(|i| request(&format!("s{i}"), i, STD_MIN, STD_MAX))
            .collect();
        let alloc = allocate_cpu(8000, &reqs).expect("allocates");
        for i in 0..4 {
            assert_eq!(alloc[&format!("s{i}")], 2000, "slot {i}");
        }
        assert_eq!(sum(&alloc), 8000);
    }

    #[test]
    fn four_standard_over_constrained_budget_share_fairly() {
        let reqs: Vec<_> = (0..4)
            .map(|i| request(&format!("s{i}"), i, STD_MIN, STD_MAX))
            .collect();
        let alloc = allocate_cpu(6000, &reqs).expect("allocates");
        for i in 0..4 {
            assert_eq!(alloc[&format!("s{i}")], 1500, "slot {i}");
        }
        assert_eq!(sum(&alloc), 6000);
    }

    #[test]
    fn economy_surplus_flows_to_standard() {
        // 3 standard + 1 economy over 6000m: economy holds 1000, the 5000 left
        // splits 3 ways as 1667/1667/1666 (residue to lowest slots).
        let reqs = vec![
            request("a", 0, STD_MIN, STD_MAX),
            request("b", 1, STD_MIN, STD_MAX),
            request("c", 2, STD_MIN, STD_MAX),
            request("eco", 3, ECO, ECO),
        ];
        let alloc = allocate_cpu(6000, &reqs).expect("allocates");
        assert_eq!(alloc["eco"], 1000);
        assert_eq!(alloc["a"], 1667);
        assert_eq!(alloc["b"], 1667);
        assert_eq!(alloc["c"], 1666);
        assert_eq!(sum(&alloc), 6000);
    }

    #[test]
    fn residue_goes_to_lowest_slot_indexes() {
        // 8003m over four standard: 2000 each would be 8000; the +3 residue lands
        // on the three lowest slots — but each is already capped at 2000, so it
        // cannot, and the sum stays at 8000. Verifies caps beat residue.
        let reqs: Vec<_> = (0..4)
            .map(|i| request(&format!("s{i}"), i, STD_MIN, STD_MAX))
            .collect();
        let alloc = allocate_cpu(8003, &reqs).expect("allocates");
        assert_eq!(sum(&alloc), 8000, "caps bound the total below budget");
    }

    #[test]
    fn residue_distribution_when_uncapped() {
        // Three requests, generous caps, budget 3001+3000 floor: 1m residue must
        // land on the single lowest slot, deterministically.
        let reqs = vec![
            request("a", 2, 1000, 5000),
            request("b", 0, 1000, 5000),
            request("c", 1, 1000, 5000),
        ];
        let alloc = allocate_cpu(4000, &reqs).expect("allocates"); // 1000 residue over 3 → 334/333/333
        assert_eq!(sum(&alloc), 4000);
        // 1000 / 3 = 333 each, residue 1 → lowest slot (b, slot 0) gets +1.
        assert_eq!(alloc["b"], 1000 + 334);
        assert_eq!(alloc["c"], 1000 + 333);
        assert_eq!(alloc["a"], 1000 + 333);
    }

    #[test]
    fn insufficient_minimum_capacity_is_rejected() {
        // Nine standard sessions each need 1000m floor = 9000 > 8000 budget.
        let reqs: Vec<_> = (0..9)
            .map(|i| request(&format!("s{i}"), i, STD_MIN, STD_MAX))
            .collect();
        let err = allocate_cpu(8000, &reqs).expect_err("must reject");
        assert_eq!(
            err,
            CpuAllocationError::InsufficientMinimumCapacity {
                budget_millis: 8000,
                requested_min_millis: 9000,
            }
        );
    }

    #[test]
    fn input_order_does_not_change_result() {
        let forward = vec![
            request("a", 0, STD_MIN, STD_MAX),
            request("b", 1, STD_MIN, STD_MAX),
            request("c", 2, STD_MIN, STD_MAX),
            request("eco", 3, ECO, ECO),
        ];
        let mut reversed = forward.clone();
        reversed.reverse();
        assert_eq!(
            allocate_cpu(6000, &forward).unwrap(),
            allocate_cpu(6000, &reversed).unwrap(),
            "allocation must depend only on slot_index, not input order"
        );
    }

    #[test]
    fn freed_session_lets_survivors_grow() {
        // The core rebalance the PR 2 manager relies on: four standard sessions
        // over a 6000m budget each get a constrained 1500m; when one ends, the
        // remaining three re-run against the same budget and each rises to its
        // 2000m ceiling. No request field changes between the two calls — only
        // the membership of the active set.
        let four: Vec<_> = (0..4)
            .map(|i| request(&format!("s{i}"), i, STD_MIN, STD_MAX))
            .collect();
        let before = allocate_cpu(6000, &four).unwrap();
        for i in 0..4 {
            assert_eq!(before[&format!("s{i}")], 1500, "constrained: slot {i}");
        }

        let three: Vec<_> = (0..3)
            .map(|i| request(&format!("s{i}"), i, STD_MIN, STD_MAX))
            .collect();
        let after = allocate_cpu(6000, &three).unwrap();
        for i in 0..3 {
            assert_eq!(after[&format!("s{i}")], 2000, "freed budget: slot {i}");
        }
        assert_eq!(sum(&after), 6000);
    }

    #[test]
    fn empty_request_set_is_empty_allocation() {
        assert!(allocate_cpu(8000, &[]).unwrap().is_empty());
    }

    #[test]
    fn allocation_never_exceeds_budget_property() {
        // Sweep a range of budgets and mixed request sets; the sum must never
        // exceed the budget and each lease must sit within [min, max].
        for budget in [1000u32, 3000, 6000, 8000, 9999, 12000] {
            let reqs = vec![
                request("a", 0, 1000, 2000),
                request("b", 1, 1000, 1000),
                request("c", 2, 1000, 4000),
                request("d", 3, 1000, 2000),
            ];
            let min_total: u32 = reqs.iter().map(|r| r.min_millis).sum();
            match allocate_cpu(budget, &reqs) {
                Ok(alloc) => {
                    assert!(sum(&alloc) <= budget, "budget {budget}");
                    for r in &reqs {
                        let got = alloc[&r.lease_id];
                        assert!(
                            got >= r.min_millis && got <= r.max_millis,
                            "budget {budget}"
                        );
                    }
                }
                Err(CpuAllocationError::InsufficientMinimumCapacity { .. }) => {
                    assert!(budget < min_total, "rejected only when below floor sum");
                }
                Err(other) => panic!("unexpected error: {other}"),
            }
        }
    }

    #[test]
    fn malformed_requests_are_rejected() {
        let zero_min = allocate_cpu(8000, &[request("a", 0, 0, 1000)]).expect_err("zero min");
        assert!(matches!(
            zero_min,
            CpuAllocationError::InvalidRequest { .. }
        ));

        let inverted = allocate_cpu(8000, &[request("a", 0, 2000, 1000)]).expect_err("min > max");
        assert!(matches!(
            inverted,
            CpuAllocationError::InvalidRequest { .. }
        ));

        let dup_slot = allocate_cpu(
            8000,
            &[request("a", 0, 1000, 2000), request("b", 0, 1000, 2000)],
        )
        .expect_err("duplicate slot");
        assert!(matches!(
            dup_slot,
            CpuAllocationError::InvalidRequest { .. }
        ));
    }

    #[test]
    fn duplicate_lease_id_is_rejected() {
        let err = allocate_cpu(
            4000,
            &[
                request("same", 0, 1000, 2000),
                request("same", 1, 1000, 2000),
            ],
        )
        .expect_err("duplicate lease_id must fail closed");
        assert_eq!(
            err,
            CpuAllocationError::DuplicateLeaseId {
                lease_id: "same".to_string(),
            }
        );
    }

    #[test]
    fn non_uniform_floor_is_rejected() {
        // v1 requires one shared floor; 1000 vs 1500 must fail rather than be
        // silently mis-shared.
        let err = allocate_cpu(
            3000,
            &[request("a", 0, 1000, 2000), request("b", 1, 1500, 2000)],
        )
        .expect_err("mixed floors must fail closed");
        assert!(matches!(err, CpuAllocationError::NonUniformMinimum { .. }));
    }
}
