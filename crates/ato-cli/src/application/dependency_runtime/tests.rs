//! Cross-module integration tests for the dependency runtime.

use std::time::Duration;

use super::endpoint::EndpointAllocator;
use super::ready::{wait_for_ready, ReadyOutcome, ReadyProbeKind};

#[test]
fn allocated_port_is_immediately_ready_after_bind() {
    // End-to-end: allocate a port, then immediately have a listener bind
    // to it. The TCP ready probe should succeed shortly after.
    let alloc = EndpointAllocator::new();
    let port = alloc.allocate_tcp().expect("allocate");

    // Spawn a listener thread that grabs the port back. There is a race:
    // between alloc.allocate_tcp dropping its temporary listener and
    // this bind, the OS could in theory hand the port to someone else,
    // but in practice nothing is competing.
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("listener");
    std::thread::spawn(move || {
        let _ = listener.accept();
    });

    let kind = ReadyProbeKind::Tcp {
        host: "127.0.0.1".to_string(),
        port,
    };
    let outcome =
        wait_for_ready(&kind, Duration::from_secs(1), Duration::from_millis(10)).expect("ready");
    matches!(outcome, ReadyOutcome::Ready { .. });
}
