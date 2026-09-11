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
//!                                                      ▼  projection
//!                                        ProgramIntent / EffectiveBuildPlan
//!                                                      │
//!                                                      ▼  execute (worker)
//!                                                 candidate C'
//!                                                      │
//!                                                      ▼  verify
//!                                                   C' ⊨ K
//! ```
//!
//! A Capsule's identity is the canonical Contract. `ProgramIntent` and
//! `EffectiveBuildPlan` sit BELOW the line: they are the projection of a
//! Derivation onto the execution machinery this worker already has, and are
//! never inputs to identity.

pub mod authoring;
pub mod capsule_toml;
pub mod detect;
pub mod intent;
pub mod preset;
pub mod projection;
pub mod source;
pub mod verify;
