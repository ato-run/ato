// Shared timeout policy for the automation transport.
//
// Compiled into both `ato-desktop` (via `mod policy;` under `automation`) and
// `ato-desktop-mcp` (via `#[path = "../automation/policy.rs"] mod policy;`).
// Each binary uses a subset of the constants, so dead_code warnings would fire
// from the binary that doesn't reach a particular constant — silence at the
// module level.
#![allow(dead_code)]

use std::time::Duration;

/// Socket read/write timeout while one side is actively exchanging bytes.
pub const AUTOMATION_CONNECTION_IO_TIMEOUT: Duration = Duration::from_secs(10);

/// Budget for the desktop process to dequeue, dispatch, and finish one
/// automation request.
pub const AUTOMATION_DISPATCH_TIMEOUT: Duration = Duration::from_secs(35);

/// MCP-side read timeout. Must strictly exceed the desktop dispatch budget so
/// the desktop times out first and owns the error contract.
pub const AUTOMATION_CLIENT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(45);

/// MCP-side write timeout. Kept short because requests are tiny and should fail
/// fast on a wedged socket.
pub const AUTOMATION_CLIENT_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Implicit grace period for MCP commands that require a loaded page.
/// This lets a fast `navigate -> click` sequence behave more like manual UI
/// interaction without special-casing each command at dispatch sites.
pub const MCP_IMPLICIT_PAGE_LOAD_TIMEOUT: Duration = Duration::from_secs(5);

// MCP read timeout must outlast desktop dispatch so the desktop emits the
// authoritative error envelope; the +5s margin also covers connection IO.
const _: () = assert!(
    AUTOMATION_CLIENT_RESPONSE_TIMEOUT.as_secs()
        >= AUTOMATION_DISPATCH_TIMEOUT.as_secs() + AUTOMATION_CONNECTION_IO_TIMEOUT.as_secs() / 2
);
