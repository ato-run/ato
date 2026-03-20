use super::*;

pub(super) struct LocalRegistryService {
    state_dir: PathBuf,
}

impl LocalRegistryService {
    pub(super) fn new(state: &AppState) -> Self {
        Self {
            state_dir: state.data_dir.join("state"),
        }
    }

    pub(super) fn list_persistent_states(
        &self,
        owner_scope: Option<&str>,
        state_name: Option<&str>,
    ) -> Result<Vec<crate::registry_store::PersistentStateRecord>> {
        self.open_state_store()?
            .list_persistent_states(owner_scope, state_name)
    }

    pub(super) fn get_persistent_state(
        &self,
        state_id: &str,
    ) -> Result<Option<crate::registry_store::PersistentStateRecord>> {
        self.open_state_store()?
            .find_persistent_state_by_id(state_id)
    }

    pub(super) fn register_persistent_state(
        &self,
        manifest_path: &Path,
        state_name: &str,
        path: &str,
    ) -> Result<crate::registry_store::PersistentStateRecord> {
        let manifest = load_manifest(manifest_path)?;
        let store = self.open_state_store()?;
        ensure_registered_state_binding_in_store(&manifest, state_name, path, &store)
    }

    pub(super) fn list_service_bindings(
        &self,
        owner_scope: Option<&str>,
        service_name: Option<&str>,
    ) -> Result<Vec<crate::registry_store::ServiceBindingRecord>> {
        binding::open_binding_store()?.list_service_bindings(owner_scope, service_name)
    }

    pub(super) fn get_service_binding(
        &self,
        binding_id: &str,
    ) -> Result<Option<crate::registry_store::ServiceBindingRecord>> {
        binding::open_binding_store()?.find_service_binding_by_id(binding_id)
    }

    pub(super) fn resolve_service_binding(
        &self,
        owner_scope: &str,
        service_name: &str,
        binding_kind: &str,
        caller_service: Option<&str>,
    ) -> Result<crate::registry_store::ServiceBindingRecord> {
        binding::resolve_binding_record(owner_scope, service_name, binding_kind, caller_service)
    }

    fn open_state_store(&self) -> Result<RegistryStore> {
        RegistryStore::open(&self.state_dir)
    }
}
