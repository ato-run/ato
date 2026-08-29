//! Registries a local execution needs that are the same on every host.
//!
//! Adapters, Record schemas and Contract verifiers carry no physical-backend
//! dependency, so they are constructed here. The MATERIALIZER registry is not:
//! it is the one that can contain VM Snapshot, whose physical backend is
//! platform-specific (Firecracker on Linux today), so the composition root
//! supplies it — see `MaterializerFactory` in this crate's root.

use std::sync::Arc;

use anyhow::Result;
use ato_adapter_api::{
    ADAPTER_ADD_OPERATION, ADAPTER_CONFIGURE_OPERATION, ADAPTER_PROTOCOL_ID,
    ADAPTER_REMOVE_OPERATION, AdapterControlPayload, AdapterRegistry, SupportedOperation,
    decode_adapter_control_payload,
};
use ato_adapter_binding::{
    BINDING_ATTACH_OPERATION, BINDING_DETACH_OPERATION, BINDING_PROTOCOL_ID,
    BINDING_REPLACE_OPERATION, BindingAdapter, BindingEvent, decode_event as decode_binding_event,
};
use ato_adapter_browser::{
    BrowserAdapter, register_record_schemas as register_browser_record_schemas,
};
use ato_adapter_http::{
    HTTP_PROTOCOL_ID, HTTP_REQUEST_OPERATION, HttpAdapter, HttpEvent,
    decode_event as decode_http_event,
};
use ato_adapter_process::ProcessLifecycleAdapter;
use ato_adapter_pty::{
    PTY_INPUT_OPERATION, PTY_PROTOCOL_ID, PTY_RESIZE_OPERATION, PTY_SIGNAL_OPERATION, PtyAdapter,
    PtyEvent, decode_event as decode_pty_event,
};
use ato_adapter_workspace::{
    WORKSPACE_DELETE_OPERATION, WORKSPACE_PROTOCOL_ID, WORKSPACE_PUT_OPERATION,
    WORKSPACE_RENAME_OPERATION, WorkspaceAdapter, WorkspaceMutation, decode_mutation,
};
use ato_contracts::{HttpEndpointVerifier, WorkspaceContentVerifier};
use ato_materializer_api::{ContractVerifierRegistry, MaterializerRegistry};
use ato_materializer_replay::{ReplayMaterializer, ReplayMaterializerV2};
use ato_materializer_snapshot::{SnapshotMaterializer, WorkspaceSnapshotMaterializer};
use ato_record_writer::RecordSchemaRegistry;

pub fn adapter_registry() -> Result<AdapterRegistry> {
    let mut registry = AdapterRegistry::default();
    registry.register(Arc::new(ProcessLifecycleAdapter))?;
    registry.register(Arc::new(PtyAdapter))?;
    registry.register(Arc::new(WorkspaceAdapter))?;
    registry.register(Arc::new(BindingAdapter))?;
    registry.register(Arc::new(HttpAdapter))?;
    registry.register(Arc::new(BrowserAdapter))?;
    Ok(registry)
}

pub fn record_schema_registry() -> Result<RecordSchemaRegistry> {
    let mut registry = RecordSchemaRegistry::default();
    registry.register(
        operation(HTTP_PROTOCOL_ID, HTTP_REQUEST_OPERATION),
        |bytes| match decode_http_event(bytes).map_err(|error| error.to_string())? {
            HttpEvent::Request { .. } => Ok(()),
            HttpEvent::Response { .. } => Err("HTTP responses are runtime output".to_owned()),
        },
    )?;
    register_browser_record_schemas(&mut registry)?;
    for operation_id in [
        PTY_INPUT_OPERATION,
        PTY_RESIZE_OPERATION,
        PTY_SIGNAL_OPERATION,
    ] {
        registry.register(operation(PTY_PROTOCOL_ID, operation_id), move |bytes| {
            let event = decode_pty_event(bytes).map_err(|error| error.to_string())?;
            let actual = match event {
                PtyEvent::Input { .. } => PTY_INPUT_OPERATION,
                PtyEvent::Resize { .. } => PTY_RESIZE_OPERATION,
                PtyEvent::Signal { .. } => PTY_SIGNAL_OPERATION,
                PtyEvent::Output { .. } | PtyEvent::Attach | PtyEvent::Detach => {
                    return Err("PTY output and lifecycle observations are not Records".to_owned());
                }
            };
            (actual == operation_id)
                .then_some(())
                .ok_or_else(|| format!("PTY payload kind does not match `{operation_id}`"))
        })?;
    }
    for operation_id in [
        BINDING_ATTACH_OPERATION,
        BINDING_REPLACE_OPERATION,
        BINDING_DETACH_OPERATION,
    ] {
        registry.register(operation(BINDING_PROTOCOL_ID, operation_id), move |bytes| {
            let event = decode_binding_event(bytes).map_err(|error| error.to_string())?;
            let actual = match event {
                BindingEvent::Attach { .. } => BINDING_ATTACH_OPERATION,
                BindingEvent::Replace { .. } => BINDING_REPLACE_OPERATION,
                BindingEvent::Detach { .. } => BINDING_DETACH_OPERATION,
            };
            (actual == operation_id)
                .then_some(())
                .ok_or_else(|| format!("Binding payload kind does not match `{operation_id}`"))
        })?;
    }
    for operation_id in [
        WORKSPACE_PUT_OPERATION,
        WORKSPACE_DELETE_OPERATION,
        WORKSPACE_RENAME_OPERATION,
    ] {
        registry.register(
            operation(WORKSPACE_PROTOCOL_ID, operation_id),
            move |bytes| {
                let mutation = decode_mutation(bytes).map_err(|error| error.to_string())?;
                let actual = match mutation {
                    WorkspaceMutation::Put { .. } => WORKSPACE_PUT_OPERATION,
                    WorkspaceMutation::Delete { .. } => WORKSPACE_DELETE_OPERATION,
                    WorkspaceMutation::Rename { .. } => WORKSPACE_RENAME_OPERATION,
                };
                (actual == operation_id).then_some(()).ok_or_else(|| {
                    format!("Workspace payload kind does not match `{operation_id}`")
                })
            },
        )?;
    }
    for operation_id in [
        ADAPTER_ADD_OPERATION,
        ADAPTER_REMOVE_OPERATION,
        ADAPTER_CONFIGURE_OPERATION,
    ] {
        registry.register(operation(ADAPTER_PROTOCOL_ID, operation_id), move |bytes| {
            let payload =
                decode_adapter_control_payload(bytes).map_err(|error| error.to_string())?;
            let actual = match payload {
                AdapterControlPayload::Add { .. } => ADAPTER_ADD_OPERATION,
                AdapterControlPayload::Remove { .. } => ADAPTER_REMOVE_OPERATION,
                AdapterControlPayload::Configure { .. } => ADAPTER_CONFIGURE_OPERATION,
            };
            (actual == operation_id)
                .then_some(())
                .ok_or_else(|| format!("Adapter payload kind does not match `{operation_id}`"))
        })?;
    }
    Ok(registry)
}

pub fn contract_verifier_registry() -> Result<ContractVerifierRegistry> {
    let mut registry = ContractVerifierRegistry::default();
    registry.register(Arc::new(HttpEndpointVerifier))?;
    registry.register(Arc::new(WorkspaceContentVerifier))?;
    Ok(registry)
}

pub(crate) fn operation(protocol_id: &str, operation_id: &str) -> SupportedOperation {
    SupportedOperation::new(protocol_id, operation_id, 1, Default::default())
        .expect("built-in Record operation identifiers are valid")
}

/// The materializers every host can realize, whatever hardware it has.
///
/// Replay and Snapshot need no hypervisor, so they belong here. VM Snapshot
/// does not: its physical backend is platform-specific, so a composition root
/// that HAS one adds it to this set — see `MaterializerFactory`.
pub fn core_materializer_registry() -> Result<MaterializerRegistry> {
    let mut registry = MaterializerRegistry::default();
    registry.register(Arc::new(ReplayMaterializer))?;
    registry.register(Arc::new(ReplayMaterializerV2))?;
    registry.register(Arc::new(SnapshotMaterializer))?;
    registry.register(Arc::new(WorkspaceSnapshotMaterializer))?;
    Ok(registry)
}
