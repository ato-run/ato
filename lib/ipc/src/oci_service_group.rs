//! Bounds of an OCI service group: several `ato.oci@1` serving steps of ONE
//! Derivation, realized together on one Runner.
//!
//! These numbers are the single authority for both judges of a group — the
//! portable profile that validates a bound Derivation and the launch spec a
//! Runner re-validates — so the two can never admit different shapes.

/// Fewest serving steps that make a group; one step is the ordinary OCI route.
pub const OCI_SERVICE_GROUP_MIN_SERVICES: usize = 2;
pub const OCI_SERVICE_GROUP_MAX_SERVICES: usize = 4;
/// Ports across every service of the group, Surface included.
pub const OCI_SERVICE_GROUP_MAX_PORTS: usize = 8;

/// Aggregate ceilings. Each service's own limit is bounded by the same value,
/// and the group's sum must not exceed it either.
pub const OCI_SERVICE_GROUP_MAX_MEMORY_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const OCI_SERVICE_GROUP_MAX_CPU_MILLIS: u64 = 4_000;
pub const OCI_SERVICE_GROUP_MAX_PIDS: u64 = 1_024;

/// Runner heartbeat capability. A group is a capability of the OCI evaluator,
/// not a separate execution ABI, so it is advertised beside `execution_abi=oci`.
pub const OCI_SERVICE_GROUP_RUNTIME_FEATURE: &str = "runtime_feature=oci_service_group_v1";

/// A service name becomes the container's network alias, so sibling services
/// resolve it by DNS. It must therefore be one RFC 1123 label: lowercase
/// alphanumerics and inner hyphens, starting with a letter.
pub fn is_service_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=63).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[bytes.len() - 1] != b'-'
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

/// Summed resource limits of a group, or `None` when one service or the sum
/// exceeds a ceiling, a limit is zero, or the sum overflows.
pub fn group_totals(limits: impl IntoIterator<Item = (u64, u64, u64)>) -> Option<(u64, u64, u64)> {
    let mut total = (0u64, 0u64, 0u64);
    for (memory, cpu, pids) in limits {
        if memory == 0
            || cpu == 0
            || pids == 0
            || memory > OCI_SERVICE_GROUP_MAX_MEMORY_BYTES
            || cpu > OCI_SERVICE_GROUP_MAX_CPU_MILLIS
            || pids > OCI_SERVICE_GROUP_MAX_PIDS
        {
            return None;
        }
        total = (
            total.0.checked_add(memory)?,
            total.1.checked_add(cpu)?,
            total.2.checked_add(pids)?,
        );
    }
    (total.0 <= OCI_SERVICE_GROUP_MAX_MEMORY_BYTES
        && total.1 <= OCI_SERVICE_GROUP_MAX_CPU_MILLIS
        && total.2 <= OCI_SERVICE_GROUP_MAX_PIDS)
        .then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_names_are_single_dns_labels() {
        for valid in ["web", "stalwart", "a1-b2"] {
            assert!(is_service_name(valid), "{valid}");
        }
        for invalid in ["", "Web", "1web", "web-", "we.b", "we_b", &"a".repeat(64)] {
            assert!(!is_service_name(invalid), "{invalid}");
        }
    }

    #[test]
    fn totals_refuse_zero_single_and_aggregate_overruns() {
        let gib = 1024 * 1024 * 1024;
        assert_eq!(
            group_totals([(gib, 1_500, 128), (gib, 1_500, 128)]),
            Some((2 * gib, 3_000, 256))
        );
        assert_eq!(group_totals([(gib, 0, 128)]), None);
        assert_eq!(group_totals([(5 * gib, 1_000, 128)]), None);
        assert_eq!(group_totals([(gib, 2_500, 128), (gib, 2_500, 128)]), None);
        assert_eq!(group_totals([(u64::MAX, 1, 1)]), None);
    }
}
