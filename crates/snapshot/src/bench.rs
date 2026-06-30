//! Opt-in latency instrumentation for the Ready-State path.
//!
//! Active **only** when `ATO_READY_STATE_BENCH=1` (or `true`). When off — the
//! product default — every hook is a single relaxed atomic load and a no-op, so
//! the build/restore path is unchanged. When on, named spans are recorded to a
//! thread-local buffer that a benchmark harness drains after each operation, to
//! decompose where snapshot build/restore time actually goes (raw Firecracker
//! vs. Ato rehydrate/cache/store/scan overhead).

use std::cell::RefCell;
use std::sync::atomic::{AtomicI8, Ordering};
use std::time::{Duration, Instant};

// -1 = not yet read from env, 0 = off, 1 = on.
static STATE: AtomicI8 = AtomicI8::new(-1);

fn enabled() -> bool {
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        0 => false,
        _ => {
            let on = std::env::var("ATO_READY_STATE_BENCH")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            STATE.store(if on { 1 } else { 0 }, Ordering::Relaxed);
            on
        }
    }
}

/// Whether benchmark instrumentation is active for this process.
pub fn is_enabled() -> bool {
    enabled()
}

/// A single recorded timing span (microsecond resolution).
#[derive(Clone, Copy, Debug)]
pub struct Span {
    pub name: &'static str,
    pub micros: u128,
}

thread_local! {
    static SPANS: RefCell<Vec<Span>> = const { RefCell::new(Vec::new()) };
}

/// Record a pre-measured span (no-op when instrumentation is off).
pub fn record(name: &'static str, dur: Duration) {
    if enabled() {
        SPANS.with(|s| s.borrow_mut().push(Span { name, micros: dur.as_micros() }));
    }
}

/// Time `f`, recording its duration under `name`. When instrumentation is off
/// this is exactly `f()` with no `Instant` reads — zero added cost.
pub fn time<T>(name: &'static str, f: impl FnOnce() -> T) -> T {
    if !enabled() {
        return f();
    }
    let t = Instant::now();
    let r = f();
    record(name, t.elapsed());
    r
}

/// Take and clear the spans recorded on this thread since the last drain.
pub fn drain() -> Vec<Span> {
    SPANS.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

#[cfg(test)]
pub(crate) fn force_enabled(on: bool) {
    STATE.store(if on { 1 } else { 0 }, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test: `STATE` is process-global, so off/on phases must not race across
    // parallel tests.
    #[test]
    fn off_then_on_recording() {
        // OFF: time/record are no-ops and return the closure value.
        force_enabled(false);
        let _ = drain(); // clear any residue
        assert_eq!(time("noop", || 7), 7);
        record("manual", Duration::from_millis(5));
        assert!(drain().is_empty(), "no spans recorded when disabled");

        // ON: spans are recorded and drain clears the buffer.
        force_enabled(true);
        let _ = drain();
        time("a", std::thread::yield_now);
        record("b", Duration::from_micros(123));
        let spans = drain();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[1].name, "b");
        assert_eq!(spans[1].micros, 123);
        assert!(drain().is_empty(), "drain clears the buffer");
        force_enabled(false);
    }
}
