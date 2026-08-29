//! Local execution of Ato Computations.
//!
//! This is the composition that turns Ato's execution primitives — Kernel,
//! ObjectStore, adapters, materializers, the Record pipeline — into a running
//! local Computation. It previously lived inside the `ato` CLI as private
//! modules, which meant executing a Computation locally required being the CLI.
//! The code is unchanged; only its address is.
//!
//! # Why the materializer registry is injected
//!
//! VM Snapshot is an Ato SEMANTIC capability. Firecracker is one PHYSICAL
//! backend for it, on Linux. Constructing the materializer registry in here
//! would weld the two together and force every consumer to link a Linux
//! hypervisor — including a macOS Desktop runtime that only ever realizes
//! source/replay Computations and will never boot a microVM.
//!
//! So the composition root supplies it:
//!
//! - the CLI passes its full registry, VM Snapshot included, so its behaviour
//!   is exactly what it was;
//! - a Desktop local runtime passes only what it can actually realize.
//!
//! A future macOS VM Snapshot backend (Virtualization.framework) plugs in at
//! this same seam, as another physical backend behind the same semantic
//! capability — without this crate learning about either.

#![forbid(unsafe_code)]

pub mod authoring;
pub mod registry;
pub mod supervisor;

use anyhow::Result;
use ato_materializer_api::MaterializerRegistry;

pub use registry::{adapter_registry, contract_verifier_registry, record_schema_registry};
pub use supervisor::{
    LocalRealizationDriver, preflight_actuator_provider_registry, start_durable, stop_active,
    worker,
};

/// Supplies the materializers a consumer can actually realize with.
///
/// A factory rather than a value because the existing code builds a fresh
/// registry at each use; keeping that shape means this extraction changes no
/// lifetimes or sharing semantics, only who decides the contents.
pub type MaterializerFactory<'a> = &'a (dyn Fn() -> Result<MaterializerRegistry> + Send + Sync);
