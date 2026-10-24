//! Bounded receive-only text paste for an explicitly capable isolated display.
//! This is not used by the byte-transparent legacy gateway.
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;

pub(crate) const MAX_PASTE_BYTES: usize = 64 * 1024;
pub(crate) const ASSEMBLY_TIMEOUT: Duration = Duration::from_secs(5);
const PASTE_INTERVAL: Duration = Duration::from_secs(1);
const MAX_MESSAGE_BYTES: usize = MAX_PASTE_BYTES + 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rejected;

pub(crate) type PasteRateLimit = Arc<Mutex<Option<Instant>>>;

pub(crate) struct ClientFilter {
    handshake: u8,
    pending: Vec<u8>,
    started: Option<Instant>,
    paste_until: Option<u64>,
    rate_limit: PasteRateLimit,
    paste_activity: bool,
}
impl ClientFilter {
    pub(crate) fn new(paste_until: Option<u64>) -> Self {
        Self {
            handshake: 0,
            pending: Vec::new(),
            started: None,
            paste_until,
            rate_limit: Arc::new(Mutex::new(None)),
            paste_activity: false,
        }
    }
    pub(crate) fn with_rate_limit(mut self, limit: PasteRateLimit) -> Self {
        self.rate_limit = limit;
        self
    }
    pub(crate) fn take_paste_activity(&mut self) -> bool {
        std::mem::take(&mut self.paste_activity)
    }
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.started.map(|started| started + ASSEMBLY_TIMEOUT)
    }
    pub(crate) fn feed(
        &mut self,
        mut bytes: &[u8],
        now: Instant,
        unix_seconds: u64,
    ) -> Result<Vec<u8>, Rejected> {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            return Err(Rejected);
        }
        let mut forwarded = Vec::new();
        while !bytes.is_empty() {
            let length = self.length(unix_seconds)?;
            if self.pending.len() < length {
                let take = (length - self.pending.len()).min(bytes.len());
                if self.pending.is_empty() {
                    self.started = Some(now);
                }
                self.pending.extend_from_slice(&bytes[..take]);
                bytes = &bytes[take..];
            }
            let length = self.length(unix_seconds)?;
            if self.pending.len() == length {
                self.validate(now)?;
                forwarded.extend_from_slice(&self.pending);
                self.pending.clear();
                self.started = None;
            }
        }
        Ok(forwarded)
    }
    fn length(&self, unix_seconds: u64) -> Result<usize, Rejected> {
        if self.handshake < 3 {
            return Ok(if self.handshake == 0 { 12 } else { 1 });
        }
        let Some(kind) = self.pending.first() else {
            return Ok(1);
        };
        let length = match kind {
            0 => 20, // SetPixelFormat
            2 => {
                // SetEncodings
                if self.pending.len() < 4 {
                    return Ok(4);
                }
                4 + 4 * usize::from(u16::from_be_bytes([self.pending[2], self.pending[3]]))
            }
            3 => 10, // FramebufferUpdateRequest
            4 => 8,  // KeyEvent
            5 => 6,  // PointerEvent
            6 => {
                // ClientCutText: only positive RFC 6143 Latin-1, never extended clipboard.
                if self.paste_until.is_none_or(|expiry| unix_seconds >= expiry) {
                    return Err(Rejected);
                }
                if self.pending.len() < 8 {
                    return Ok(8);
                }
                if self.pending[1..4] != [0, 0, 0] {
                    return Err(Rejected);
                }
                let length =
                    i32::from_be_bytes(self.pending[4..8].try_into().map_err(|_| Rejected)?);
                if length < 0 || length as usize > MAX_PASTE_BYTES {
                    return Err(Rejected);
                }
                8 + length as usize
            }
            150 => 10, // EnableContinuousUpdates
            248 => {
                // ClientFence
                if self.pending.len() < 9 {
                    return Ok(9);
                }
                9 + usize::from(self.pending[8])
            }
            250 => 4, // XvpOp
            251 => {
                // SetDesktopSize: bounded variable screen list, not a fixed 24 bytes.
                if self.pending.len() < 8 {
                    return Ok(8);
                }
                let screens = usize::from(self.pending[6]);
                if screens == 0 || screens > 16 {
                    return Err(Rejected);
                }
                8 + 16 * screens
            }
            255 => 12, // QEMUExtendedKeyEvent
            _ => return Err(Rejected),
        };
        if length > MAX_MESSAGE_BYTES {
            return Err(Rejected);
        }
        Ok(length)
    }
    fn validate(&mut self, now: Instant) -> Result<(), Rejected> {
        if self.handshake < 3 {
            match self.handshake {
                0 if self.pending != b"RFB 003.008\n" => return Err(Rejected),
                1 if self.pending != [1] => return Err(Rejected),
                2 if self.pending[0] > 1 => return Err(Rejected),
                _ => {}
            }
            self.handshake += 1;
        } else if self.pending[0] == 6 {
            // ISO-8859-1 bytes are preserved. Deny C0/DEL/C1 control payloads.
            if self.pending[8..]
                .iter()
                .any(|b| (*b < 32 && ![9, 10, 13].contains(b)) || (127..=159).contains(b))
            {
                return Err(Rejected);
            }
            let mut last = self.rate_limit.lock().map_err(|_| Rejected)?;
            if last.is_some_and(|last| now.saturating_duration_since(last) < PASTE_INTERVAL) {
                return Err(Rejected);
            }
            *last = Some(now);
            self.paste_activity = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(text: &[u8]) -> Vec<u8> {
        let mut bytes = vec![6, 0, 0, 0];
        bytes.extend_from_slice(&(text.len() as u32).to_be_bytes());
        bytes.extend_from_slice(text);
        bytes
    }
    fn filter(until: Option<u64>) -> ClientFilter {
        let mut filter = ClientFilter::new(until);
        assert_eq!(
            filter
                .feed(b"RFB 003.008\n\x01\x01", Instant::now(), 100)
                .unwrap(),
            b"RFB 003.008\n\x01\x01"
        );
        filter
    }
    #[test]
    fn latin1_paste_preserves_every_fragment_without_prefix_delivery() {
        let bytes = packet(b"caf\xe9 \xff\t\n");
        for split in 1..bytes.len() {
            let mut filter = filter(Some(200));
            assert!(
                filter
                    .feed(&bytes[..split], Instant::now(), 100)
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                filter.feed(&bytes[split..], Instant::now(), 100).unwrap(),
                bytes
            );
        }
    }
    #[test]
    fn denied_and_expired_paste_reject_before_a_prefix() {
        for until in [None, Some(99), Some(100)] {
            let mut filter = filter(until);
            assert!(filter.feed(&[6], Instant::now(), 100).is_err());
            assert!(filter.pending.len() <= 1);
        }
        let mut filter = filter(Some(101));
        assert!(
            filter
                .feed(&packet(b"secret")[..9], Instant::now(), 100)
                .unwrap()
                .is_empty()
        );
        assert!(filter.feed(b"ecret", Instant::now(), 101).is_err());
    }
    #[test]
    fn negative_oversize_padding_controls_and_unknown_are_rejected() {
        let cases = [
            vec![6, 0, 0, 0, 255, 255, 255, 255],
            vec![6, 0, 0, 0, 0, 1, 0, 1],
            vec![6, 1, 0, 0, 0, 0, 0, 1, b'x'],
            packet(b"\x00"),
            packet(b"\x1b"),
            packet(b"\x7f"),
            packet(b"\x85"),
            vec![42],
        ];
        for bytes in cases {
            let mut filter = filter(Some(200));
            assert!(
                filter.feed(&bytes, Instant::now(), 100).is_err(),
                "accepted malformed packet"
            );
        }
    }
    #[test]
    fn paste_size_rate_and_absolute_partial_deadline_are_bounded() {
        let now = Instant::now();
        let mut f = filter(Some(200));
        let bytes = packet(&vec![b'a'; MAX_PASTE_BYTES]);
        assert_eq!(f.feed(&bytes, now, 100).unwrap(), bytes);
        assert!(f.pending.capacity() <= MAX_MESSAGE_BYTES * 2);
        assert!(f.feed(&packet(b"next"), now, 100).is_err());
        let mut f = filter(Some(200));
        assert_eq!(
            f.feed(&packet(b"first"), now, 100).unwrap(),
            packet(b"first")
        );
        assert_eq!(
            f.feed(&packet(b"next"), now + PASTE_INTERVAL, 101).unwrap(),
            packet(b"next")
        );
        let mut f = filter(Some(200));
        assert!(f.feed(&[6], now, 100).unwrap().is_empty());
        let deadline = f.deadline().unwrap();
        assert!(
            f.feed(&[0], now + Duration::from_secs(4), 100)
                .unwrap()
                .is_empty()
        );
        assert_eq!(f.deadline(), Some(deadline));
        assert!(f.feed(&[0], deadline, 100).is_err());
    }
    #[test]
    fn known_messages_and_handshake_remain_byte_identical() {
        let bytes = [0; 20];
        let mut f = filter(None);
        assert_eq!(f.feed(&bytes, Instant::now(), 100).unwrap(), bytes);
        let mut screens = vec![251, 0, 0, 32, 0, 32, 2, 0];
        screens.extend([0; 32]);
        assert_eq!(f.feed(&screens, Instant::now(), 100).unwrap(), screens);
        let mut f = ClientFilter::new(None);
        assert!(f.feed(b"RFB 003.003\n", Instant::now(), 100).is_err());
    }
    #[test]
    fn fresh_connection_cannot_reset_gateway_paste_budget() {
        let shared = Arc::new(Mutex::new(None));
        let now = Instant::now();
        let mut first = filter(Some(200)).with_rate_limit(Arc::clone(&shared));
        assert_eq!(
            first.feed(&packet(b"one"), now, 100).unwrap(),
            packet(b"one")
        );
        let mut reconnected = filter(Some(200)).with_rate_limit(Arc::clone(&shared));
        assert!(reconnected.feed(&packet(b"two"), now, 100).is_err());
        let mut later = filter(Some(200)).with_rate_limit(shared);
        assert_eq!(
            later
                .feed(&packet(b"two"), now + PASTE_INTERVAL, 101)
                .unwrap(),
            packet(b"two")
        );
    }
}
