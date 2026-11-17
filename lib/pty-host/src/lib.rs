//! The host pseudo-terminal primitive.
//!
//! A Terminal surface needs a real PTY: a workload that asks whether its input
//! is a terminal behaves differently when it is not, and the declared terminal
//! exchanges are defined in terms of that behavior. This crate is the smallest
//! place that can own one: it allocates the PTY, starts the already-prepared
//! command as a session leader with the slave as its controlling terminal,
//! reads bounded output and writes input.
//!
//! It is deliberately not a general shell runner: it starts exactly one
//! command with an explicitly supplied environment, and it decides nothing
//! about Contract K. The Adapter that uses it records what each exchange
//! measured.

#![deny(missing_docs)]

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::PtySession;

#[cfg(not(unix))]
mod unsupported;
#[cfg(not(unix))]
pub use unsupported::PtySession;
