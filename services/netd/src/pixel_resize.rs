//! Physical resize admission for the owned isolated RFB display. This does not
//! change the authored viewport, frozen application probe or Contract evidence.
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

pub(crate) const MAX_WIDTH: u16 = 4096;
pub(crate) const MAX_HEIGHT: u16 = 2160;
const RESIZE_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResizeRejected;

/// One limiter belongs to a gateway, rather than an individual WebSocket, so
/// reconnects and concurrent clients cannot reset the allocation frequency.
#[derive(Default)]
pub(crate) struct ResizeLimiter {
    last: Mutex<Option<Instant>>,
}
impl ResizeLimiter {
    pub(crate) fn accept(
        &self,
        message: &[u8],
        permission_until: Option<u64>,
        now: Instant,
        unix_seconds: u64,
    ) -> Result<(), ResizeRejected> {
        if permission_until.is_none_or(|expiry| unix_seconds >= expiry) {
            return Err(ResizeRejected);
        }
        validate(message)?;
        let mut last = self.last.lock().map_err(|_| ResizeRejected)?;
        if last.is_some_and(|last| now.saturating_duration_since(last) < RESIZE_INTERVAL) {
            return Err(ResizeRejected);
        }
        *last = Some(now);
        Ok(())
    }
}

fn validate(bytes: &[u8]) -> Result<(), ResizeRejected> {
    // SetDesktopSize header + exactly one 16-byte screen. Screen id is the
    // isolated server's opaque identifier; it never selects a host display.
    if bytes.len() != 24
        || bytes[0] != 251
        || bytes[1] != 0
        || bytes[6] != 1
        || bytes[7] != 0
        || bytes[12..16] != [0; 4]
        || bytes[20..24] != [0; 4]
    {
        return Err(ResizeRejected);
    }
    let width = u16::from_be_bytes([bytes[2], bytes[3]]);
    let height = u16::from_be_bytes([bytes[4], bytes[5]]);
    if !(1..=MAX_WIDTH).contains(&width)
        || !(1..=MAX_HEIGHT).contains(&height)
        || bytes[16..20] != bytes[2..6]
    {
        return Err(ResizeRejected);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn packet(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = vec![251, 0];
        bytes.extend(width.to_be_bytes());
        bytes.extend(height.to_be_bytes());
        bytes.extend([1, 0]);
        bytes.extend(0x10203040u32.to_be_bytes());
        bytes.extend([0; 4]);
        bytes.extend(width.to_be_bytes());
        bytes.extend(height.to_be_bytes());
        bytes.extend([0; 4]);
        bytes
    }

    #[test]
    fn single_screen_preserves_opaque_id_and_accepts_exact_dimension_bounds() {
        for (width, height) in [(1, 1), (1024, 768), (MAX_WIDTH, MAX_HEIGHT)] {
            let bytes = packet(width, height);
            let before = bytes.clone();
            assert_eq!(validate(&bytes), Ok(()));
            assert_eq!(bytes, before);
        }
    }

    #[test]
    fn incomplete_extra_screen_padding_layout_flags_and_oversize_are_rejected() {
        let valid = packet(1024, 768);
        for length in 0..valid.len() {
            assert_eq!(validate(&valid[..length]), Err(ResizeRejected));
        }
        let mut extra = valid.clone();
        extra.push(0);
        assert_eq!(validate(&extra), Err(ResizeRejected));
        for (index, byte) in [
            (0, 250),
            (1, 1),
            (6, 0),
            (6, 2),
            (7, 1),
            (12, 1),
            (14, 1),
            (16, 1),
            (18, 1),
            (20, 1),
            (23, 1),
        ] {
            let mut changed = valid.clone();
            changed[index] = byte;
            assert_eq!(validate(&changed), Err(ResizeRejected), "offset {index}");
        }
        for (width, height) in [(0, 768), (1024, 0), (4097, 768), (1024, 2161)] {
            assert_eq!(validate(&packet(width, height)), Err(ResizeRejected));
        }
    }

    #[test]
    fn absent_or_expired_permission_cannot_consume_the_resize_allowance() {
        let limiter = ResizeLimiter::default();
        let now = Instant::now();
        let bytes = packet(1024, 768);
        for expiry in [None, Some(99), Some(100)] {
            assert_eq!(
                limiter.accept(&bytes, expiry, now, 100),
                Err(ResizeRejected)
            );
        }
        assert_eq!(limiter.accept(&bytes, Some(101), now, 100), Ok(()));
        assert_eq!(
            limiter.accept(&bytes, Some(101), now + RESIZE_INTERVAL, 101),
            Err(ResizeRejected)
        );
    }

    #[test]
    fn limiter_is_shared_across_reconnects() {
        let limiter = Arc::new(ResizeLimiter::default());
        let reconnected = Arc::clone(&limiter);
        let concurrent = Arc::clone(&limiter);
        let now = Instant::now();
        let bytes = packet(1024, 768);
        assert_eq!(limiter.accept(&bytes, Some(200), now, 100), Ok(()));
        assert_eq!(
            reconnected.accept(&bytes, Some(200), now, 100),
            Err(ResizeRejected)
        );
        assert_eq!(
            concurrent.accept(&bytes, Some(200), now + Duration::from_millis(999), 100),
            Err(ResizeRejected)
        );
        assert_eq!(
            reconnected.accept(&bytes, Some(200), now + RESIZE_INTERVAL, 101),
            Ok(())
        );
        assert_eq!(
            limiter.accept(&bytes, Some(200), now, 100),
            Err(ResizeRejected)
        );
    }

    #[test]
    fn concurrent_clients_share_one_atomic_resize_allowance() {
        let limiter = Arc::new(ResizeLimiter::default());
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let now = Instant::now();
        let clients: Vec<_> = (0..2)
            .map(|_| {
                let limiter = Arc::clone(&limiter);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    limiter
                        .accept(&packet(1024, 768), Some(200), now, 100)
                        .is_ok()
                })
            })
            .collect();
        barrier.wait();
        let accepted = clients
            .into_iter()
            .map(|client| client.join().unwrap())
            .filter(|accepted| *accepted)
            .count();
        assert_eq!(accepted, 1);
    }

    #[test]
    fn invalid_geometry_does_not_consume_the_resize_allowance() {
        let limiter = ResizeLimiter::default();
        let now = Instant::now();
        assert_eq!(
            limiter.accept(&packet(4097, 768), Some(200), now, 100),
            Err(ResizeRejected)
        );
        assert_eq!(
            limiter.accept(&packet(1024, 768), Some(200), now, 100),
            Ok(())
        );
    }
}
