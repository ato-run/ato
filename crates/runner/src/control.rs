//! Runner-control clients — host-agnostic clients for the loopback Runtime
//! Control API and netd ingress.
//!
//! **Skeleton (Phase 1 Step 0):** the module boundary only. The concrete
//! transports migrate in from the desktop crate during Phase 1:
//! - the loopback `http://127.0.0.1:<port>` Runtime Control client from
//!   `desktop::runtime_control_client` (reads/SSE stay client-side; launch/stop
//!   go through here),
//! - the netd ingress register/deregister client from `desktop::netd`.
//!
//! Wire types come from `protocol` (`runtime_control::control`, `net::control`);
//! this module adds none of its own.
