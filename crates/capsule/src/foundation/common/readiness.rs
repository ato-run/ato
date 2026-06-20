//! Shared readiness-probe semantics.
//!
//! A manifest `readiness_probe.http_get` used to be evaluated with
//! different success criteria depending on the execution path (strict
//! 200/201/204 vs. any non-5xx, where a 404 counted as ready). All
//! probe implementations now share this single predicate.

/// Returns whether an HTTP status code observed by a
/// `readiness_probe.http_get` check indicates the service is ready.
///
/// Pinned to Kubernetes `httpGet` probe semantics: ready iff
/// `200 <= status < 400`. Redirects count as ready (the route exists and
/// the server answers); client errors such as 404 do not — a service
/// whose probe path is missing is misconfigured, not ready.
pub fn http_status_indicates_ready(status: u16) -> bool {
    (200..400).contains(&status)
}

#[cfg(test)]
mod tests {
    use super::http_status_indicates_ready;

    #[test]
    fn success_and_redirect_statuses_are_ready() {
        for status in [200, 201, 204, 301, 302, 399] {
            assert!(
                http_status_indicates_ready(status),
                "{status} must be ready"
            );
        }
    }

    #[test]
    fn informational_client_and_server_errors_are_not_ready() {
        for status in [100, 101, 199, 400, 404, 418, 500, 502, 503] {
            assert!(
                !http_status_indicates_ready(status),
                "{status} must not be ready"
            );
        }
    }
}
