//! The Formation domain: pure, and deliberately so.
//!
//! Everything here takes bytes or a tree and returns a fact. Nothing here
//! opens a socket, spawns a process, installs a dependency or touches a
//! database — those belong to the worker (`apps/formation-worker`) and the
//! control plane. Keeping the domain pure is what makes a build plan testable
//! without a build.
//!
//! ## The shape of the pipeline
//!
//! ```text
//!   capsule.toml ──┐
//!                  ├─▶ AuthoringDraft ─▶ bind ─▶ BoundContract  (ContractRef)
//!   Preset      ───┘                          └▶ BoundDerivation (DerivationRef)
//!                                                      │
//!                                                      ▼  lower(D, I facts, Runtime binding)
//!                                        minimal ExecutionPlan
//!                                                      │
//!                                                      ▼  execute (worker)
//!                                                 candidate C'
//!                                                      │
//!                                                      ▼  verify
//!                                                   C' ⊨ K
//! ```
//!
//! A Capsule's identity is the canonical Contract. `ExecutionPlan` binds D to
//! physical machinery without copying its semantics or contributing to identity.
//! The old v1 intent/build-plan codecs remain only for historical compatibility.

pub mod authoring;
pub mod browser;
pub mod capsule_toml;
#[cfg(feature = "planning")]
pub mod capsule_toml_v2;
pub mod containment;
#[cfg(feature = "planning")]
pub mod detect;
#[cfg(feature = "planning")]
pub mod execution;
#[cfg(feature = "planning")]
pub mod failure;
#[cfg(feature = "planning")]
pub mod intent;
#[cfg(feature = "planning")]
pub mod preset;
#[cfg(feature = "planning")]
pub mod projection;
pub mod receipt;
pub mod request;
pub mod retained;
#[cfg(feature = "planning")]
pub mod source;
pub mod verify;

pub mod decision;
pub mod generation;
pub mod search;
