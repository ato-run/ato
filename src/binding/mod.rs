mod contract;
mod ingress;
mod manifest;
mod register;
mod store;

#[allow(unused_imports)]
pub(crate) use contract::{
    host_service_binding_scope, SERVICE_BINDING_ADAPTER_LOCAL_SERVICE,
    SERVICE_BINDING_ADAPTER_REVERSE_PROXY, SERVICE_BINDING_KIND_INGRESS,
    SERVICE_BINDING_KIND_SERVICE, SERVICE_BINDING_TLS_MODE_DISABLED,
    SERVICE_BINDING_TLS_MODE_EXPLICIT,
};
pub(crate) use ingress::{bootstrap_ingress_tls, serve_ingress_binding};
pub(crate) use register::{
    cleanup_service_bindings_for_process_info, register_ingress_binding,
    register_ingress_binding_from_manifest, register_service_binding,
    register_service_binding_for_process, register_service_binding_from_manifest,
    register_service_binding_from_process, sync_service_bindings_for_process,
    sync_service_bindings_from_process,
};
pub(crate) use store::{
    inspect_binding, list_bindings, open_binding_store, resolve_binding, resolve_binding_record,
};

pub fn parse_binding_reference(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    trimmed.starts_with("binding-").then_some(trimmed)
}

#[cfg(test)]
mod tests;
