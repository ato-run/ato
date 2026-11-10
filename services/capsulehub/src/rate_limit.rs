//! Per-client-IP rate limiting.
//!
//! A fixed one-minute window per client address. The client address comes from
//! the accepted TCP connection (no forwarding header is trusted), so a client
//! cannot claim a different address. `0` disables the limit.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Default accounting window.
pub const WINDOW: Duration = Duration::from_secs(60);
/// When the bucket map grows past this, stale buckets are dropped.
const CLEANUP_THRESHOLD: usize = 4096;

#[derive(Debug)]
struct Bucket {
    window_start: Instant,
    count: u32,
}

/// Fixed-window limiter keyed by client IP.
#[derive(Debug)]
pub struct RateLimiter {
    limit: u32,
    window: Duration,
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
}

impl RateLimiter {
    pub fn new(limit: u32) -> Self {
        Self::with_window(limit, WINDOW)
    }

    fn with_window(limit: u32, window: Duration) -> Self {
        Self {
            limit,
            window,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Records one request and reports whether it is allowed.
    pub fn allow(&self, client: IpAddr) -> bool {
        if self.limit == 0 {
            return true;
        }
        let now = Instant::now();
        let mut buckets = self.buckets.lock().expect("rate limiter lock poisoned");
        if buckets.len() > CLEANUP_THRESHOLD {
            buckets.retain(|_, bucket| now.duration_since(bucket.window_start) < self.window * 2);
        }
        let bucket = buckets.entry(client).or_insert(Bucket {
            window_start: now,
            count: 0,
        });
        if now.duration_since(bucket.window_start) >= self.window {
            bucket.window_start = now;
            bucket.count = 0;
        }
        if bucket.count >= self.limit {
            return false;
        }
        bucket.count += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> IpAddr {
        "127.0.0.1".parse().unwrap()
    }

    #[test]
    fn limits_per_window_and_recovers_after_it() {
        let limiter = RateLimiter::with_window(2, Duration::from_millis(40));
        assert!(limiter.allow(client()));
        assert!(limiter.allow(client()));
        assert!(!limiter.allow(client()));
        std::thread::sleep(Duration::from_millis(60));
        assert!(limiter.allow(client()));
    }

    #[test]
    fn zero_disables_the_limit() {
        let limiter = RateLimiter::new(0);
        for _ in 0..1000 {
            assert!(limiter.allow(client()));
        }
    }

    #[test]
    fn limits_are_per_address() {
        let limiter = RateLimiter::new(1);
        let other: IpAddr = "::1".parse().unwrap();
        assert!(limiter.allow(client()));
        assert!(!limiter.allow(client()));
        assert!(limiter.allow(other));
    }
}
