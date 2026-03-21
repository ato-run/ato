mod contract;
mod manifest;
mod output;
mod store;

#[allow(unused_imports)]
pub(crate) use contract::{persistent_state_owner_scope, prepare_backend_locator};
#[allow(unused_imports)]
pub(crate) use manifest::{load_manifest, resolve_manifest_path};
pub(crate) use output::{inspect_state, list_states, register_state_from_manifest};
#[allow(unused_imports)]
pub(crate) use store::{
    ensure_registered_state_binding, ensure_registered_state_binding_in_store, open_state_store,
    resolve_registered_state_reference, resolve_registered_state_reference_in_store,
};

pub const PERSISTENT_STATE_KIND_FILESYSTEM: &str = "filesystem";
pub const PERSISTENT_STATE_BACKEND_KIND_HOST_PATH: &str = "host_path";
const ATO_STATE_SCHEME: &str = "ato-state://";

pub fn parse_state_reference(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix(ATO_STATE_SCHEME) {
        let state_id = rest.trim();
        return (!state_id.is_empty()).then_some(state_id);
    }
    trimmed.starts_with("state-").then_some(trimmed)
}

#[cfg(test)]
mod tests;
