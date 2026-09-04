//! Runner-local resolution of a [`RuntimeLaunchSpecV1`].
//!
//! This lives in the Runner, NOT beside the wire contract in `ato-ipc`, and
//! the split is the point. `ato-ipc` is linked by the guest agent that runs
//! *inside* the VM, and by the CLI, the desktop apps and netd — every one of
//! them needs the logical spec, and none of them should be able to name a type
//! that holds a redeemed secret or a real host path. Resolution is a Runner
//! privilege, so it is a Runner type.
//!
//! Everything the logical spec deliberately withholds lives here: real host
//! paths, redeemed secret values, the host ports that were actually allocated.
//!
//! None of these types derive `Serialize`. That is the enforcement, not a
//! convention — a resolved context cannot be put on a wire, into a receipt, or
//! into a structured log by accident, because there is no code path that turns
//! it into bytes. `Debug` is hand-written to redact, so the remaining way to
//! leak one (printing it) is also closed.
//!
//! Donor: `v0.8.0 crates/cli/src/adapters/runtime/executors/launch_context.rs`
//! and `state_binding_injection.rs` — the separation of receipt-observed
//! bindings from runtime-private ones is taken from there. The old
//! installed-app ledger, Package and Provider model around them is not.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ato_ipc::runtime_launch::{EndpointV1, RuntimeLaunchSpecError, StateAccessV1};

/// A secret value, redeemed and held only until the spawn boundary.
pub struct ResolvedSecret {
    name: String,
    value: String,
}

impl ResolvedSecret {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The only accessor for the value. Named so that every call site reads as
    /// a deliberate crossing of the boundary, and so `grep` finds all of them.
    pub fn expose_for_spawn(&self) -> &str {
        &self.value
    }
}

impl std::fmt::Debug for ResolvedSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedSecret")
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .finish()
    }
}

/// A state attachment that has been materialized into a working copy.
///
/// The logical identity (`state_key`, `revision_ref`) and the physical
/// location (`working_copy`) are separate fields on purpose: the first two are
/// safe to report, the third never is.
pub struct ResolvedStateAttachment {
    state_key: String,
    revision_ref: Option<String>,
    working_copy: PathBuf,
    guest_target: String,
    access: StateAccessV1,
}

impl ResolvedStateAttachment {
    pub fn new(
        state_key: impl Into<String>,
        revision_ref: Option<String>,
        working_copy: PathBuf,
        guest_target: impl Into<String>,
        access: StateAccessV1,
    ) -> Self {
        Self {
            state_key: state_key.into(),
            revision_ref,
            working_copy,
            guest_target: guest_target.into(),
            access,
        }
    }

    pub fn state_key(&self) -> &str {
        &self.state_key
    }

    pub fn revision_ref(&self) -> Option<&str> {
        self.revision_ref.as_deref()
    }

    pub fn guest_target(&self) -> &str {
        &self.guest_target
    }

    pub fn access(&self) -> StateAccessV1 {
        self.access
    }

    /// Runtime-private. Never reported, never digested.
    pub fn working_copy_for_mount(&self) -> &Path {
        &self.working_copy
    }

    /// What may appear in a Run receipt: identity, not location.
    pub fn observed(&self) -> ObservedStateAttachment<'_> {
        ObservedStateAttachment {
            state_key: &self.state_key,
            revision_ref: self.revision_ref.as_deref(),
            guest_target: &self.guest_target,
            access: self.access,
        }
    }
}

impl std::fmt::Debug for ResolvedStateAttachment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedStateAttachment")
            .field("state_key", &self.state_key)
            .field("revision_ref", &self.revision_ref)
            .field("guest_target", &self.guest_target)
            .field("access", &self.access)
            .field("working_copy", &"<redacted>")
            .finish()
    }
}

/// The receipt-safe projection of an attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedStateAttachment<'a> {
    pub state_key: &'a str,
    pub revision_ref: Option<&'a str>,
    pub guest_target: &'a str,
    pub access: StateAccessV1,
}

/// A logical endpoint bound to a real host port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEndpoint {
    pub name: String,
    pub guest_port: Option<u16>,
    /// Chosen by the Runner. The stable instance URL must never depend on it.
    pub host_port: u16,
}

/// Everything an executor needs and nothing it may publish.
pub struct ResolvedRuntimeLaunchContext {
    workspace_root: PathBuf,
    effective_cwd: PathBuf,
    public_env: BTreeMap<String, String>,
    secrets: Vec<ResolvedSecret>,
    state_attachments: Vec<ResolvedStateAttachment>,
    endpoints: Vec<ResolvedEndpoint>,
}

impl ResolvedRuntimeLaunchContext {
    /// Builds a context, re-checking containment against the REAL paths.
    ///
    /// The logical spec's cwd was already validated as a string, but a string
    /// check cannot see a symlink. Resolving and comparing canonical paths is
    /// what actually keeps the workload inside its workspace.
    pub fn new(
        workspace_root: PathBuf,
        cwd_relative: &str,
        public_env: BTreeMap<String, String>,
        secrets: Vec<ResolvedSecret>,
        state_attachments: Vec<ResolvedStateAttachment>,
        endpoints: Vec<ResolvedEndpoint>,
    ) -> Result<Self, RuntimeLaunchSpecError> {
        // Lexical first, and unconditionally. `canonicalize` cannot help here:
        // the workspace usually does not exist yet at resolve time, and its
        // failure path leaves `starts_with` comparing UNNORMALIZED components —
        // where `<root>/../escape` still "starts with" `<root>`. Refusing `..`
        // outright is what actually closes that, rather than relying on the
        // logical spec having already rejected it.
        let invalid = || RuntimeLaunchSpecError::InvalidCwd {
            cwd: cwd_relative.to_owned(),
        };
        if !cwd_relative.is_empty() {
            if cwd_relative.starts_with('/') || cwd_relative.contains('\0') {
                return Err(invalid());
            }
            for segment in cwd_relative.split(['/', '\\']) {
                if segment.is_empty() || segment == "." || segment == ".." {
                    return Err(invalid());
                }
            }
        }
        let effective_cwd = if cwd_relative.is_empty() {
            workspace_root.clone()
        } else {
            workspace_root.join(cwd_relative)
        };
        // Then, when both really exist, re-check against the RESOLVED paths so
        // a symlink inside the workspace cannot point out of it.
        if let (Ok(canonical_root), Ok(canonical_cwd)) =
            (workspace_root.canonicalize(), effective_cwd.canonicalize())
            && !canonical_cwd.starts_with(&canonical_root)
        {
            return Err(invalid());
        }
        Ok(Self {
            workspace_root,
            effective_cwd,
            public_env,
            secrets,
            state_attachments,
            endpoints,
        })
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn effective_cwd(&self) -> &Path {
        &self.effective_cwd
    }

    pub fn public_env(&self) -> &BTreeMap<String, String> {
        &self.public_env
    }

    pub fn state_attachments(&self) -> &[ResolvedStateAttachment] {
        &self.state_attachments
    }

    pub fn endpoints(&self) -> &[ResolvedEndpoint] {
        &self.endpoints
    }

    /// The FULL environment, secrets included. Call this at the spawn/create
    /// boundary and nowhere else — the result is what must not be logged.
    pub fn environment_for_spawn(&self) -> BTreeMap<String, String> {
        let mut environment = self.public_env.clone();
        for secret in &self.secrets {
            environment.insert(
                secret.name().to_owned(),
                secret.expose_for_spawn().to_owned(),
            );
        }
        environment
    }

    /// Secret NAMES only. This is what a receipt or diagnostic may record: it
    /// says which grants were applied without saying what they were.
    pub fn observed_secret_names(&self) -> Vec<&str> {
        self.secrets.iter().map(ResolvedSecret::name).collect()
    }

    /// Receipt-safe view of every attachment.
    pub fn observed_state(&self) -> Vec<ObservedStateAttachment<'_>> {
        self.state_attachments
            .iter()
            .map(ResolvedStateAttachment::observed)
            .collect()
    }
}

impl std::fmt::Debug for ResolvedRuntimeLaunchContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedRuntimeLaunchContext")
            .field("workspace_root", &"<redacted>")
            .field("effective_cwd", &"<redacted>")
            .field("public_env", &self.public_env)
            .field("secrets", &self.observed_secret_names())
            .field("state_attachments", &self.state_attachments)
            .field("endpoints", &self.endpoints)
            .finish()
    }
}

/// Resolving a logical spec is the Runner's job, and both realizations use the
/// same one. Keeping it a trait is what stops Process and OCI from growing two
/// different ideas of what a state attachment or an endpoint means.
pub trait RuntimeLaunchResolver {
    type Error;

    fn resolve(
        &self,
        spec: &ato_ipc::runtime_launch::RuntimeLaunchSpecV1,
    ) -> Result<ResolvedRuntimeLaunchContext, Self::Error>;
}

/// Endpoint allocation is the Runner's, not the control plane's.
pub fn allocate_endpoint(endpoint: &EndpointV1, host_port: u16) -> ResolvedEndpoint {
    ResolvedEndpoint {
        name: endpoint.name.clone(),
        guest_port: endpoint.guest_port,
        host_port,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use ato_ipc::runtime_launch::StateAccessV1;

    use super::{ResolvedRuntimeLaunchContext, ResolvedSecret, ResolvedStateAttachment};

    fn resolved_context() -> ResolvedRuntimeLaunchContext {
        let root = std::env::temp_dir();
        ResolvedRuntimeLaunchContext::new(
            root,
            "",
            BTreeMap::from([("PORT".to_owned(), "8000".to_owned())]),
            vec![ResolvedSecret::new("APP_SECRET_KEY", "hunter2")],
            vec![ResolvedStateAttachment::new(
                "app_data",
                Some("isr_1".to_owned()),
                PathBuf::from("/var/lib/ato/state/app_data/working"),
                "/data",
                StateAccessV1::ReadWrite,
            )],
            vec![],
        )
        .expect("workspace-rooted cwd resolves")
    }

    #[test]
    fn debug_redacts_secret_values_and_host_paths() {
        // The only remaining way to leak a resolved context is to print it, so
        // printing it must be safe.
        let rendered = format!("{:?}", resolved_context());
        assert!(!rendered.contains("hunter2"));
        assert!(!rendered.contains("/var/lib/ato/state"));
        // Identity survives, so a diagnostic is still worth reading.
        assert!(rendered.contains("APP_SECRET_KEY"));
        assert!(rendered.contains("app_data"));
        assert!(rendered.contains("/data"));
    }

    #[test]
    fn secrets_reach_only_the_spawn_boundary() {
        let context = resolved_context();
        assert_eq!(
            context.public_env().get("PORT").map(String::as_str),
            Some("8000")
        );
        // Not in the public env...
        assert!(!context.public_env().contains_key("APP_SECRET_KEY"));
        // ...and not in what a receipt may observe.
        assert_eq!(context.observed_secret_names(), vec!["APP_SECRET_KEY"]);
        // Only here.
        assert_eq!(
            context
                .environment_for_spawn()
                .get("APP_SECRET_KEY")
                .map(String::as_str),
            Some("hunter2")
        );
    }

    #[test]
    fn observed_state_reports_identity_without_location() {
        let context = resolved_context();
        let observed = context.observed_state();
        assert_eq!(observed[0].state_key, "app_data");
        assert_eq!(observed[0].revision_ref, Some("isr_1"));
        assert_eq!(observed[0].guest_target, "/data");
        let rendered = format!("{observed:?}");
        assert!(!rendered.contains("/var/lib/ato/state"));
    }

    #[test]
    fn a_resolved_cwd_outside_the_workspace_is_refused() {
        let error = ResolvedRuntimeLaunchContext::new(
            PathBuf::from("/tmp/ato-workspace-root"),
            "../escape",
            BTreeMap::new(),
            vec![],
            vec![],
            vec![],
        )
        .unwrap_err();
        assert_eq!(error.code(), "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_CWD");
    }
}
