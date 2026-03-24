use anyhow::Result;

pub(super) fn execute_binding_command(command: crate::BindingCommands) -> Result<()> {
    match command {
        crate::BindingCommands::List {
            owner_scope,
            service_name,
            json,
        } => crate::binding::list_bindings(owner_scope.as_deref(), service_name.as_deref(), json),
        crate::BindingCommands::Inspect { binding_ref, json } => {
            crate::binding::inspect_binding(&binding_ref, json)
        }
        crate::BindingCommands::Resolve {
            owner_scope,
            service_name,
            binding_kind,
            caller_service,
            json,
        } => crate::binding::resolve_binding(
            &owner_scope,
            &service_name,
            &binding_kind,
            caller_service.as_deref(),
            json,
        ),
        crate::BindingCommands::BootstrapTls {
            binding_ref,
            install_system_trust,
            yes,
            json,
        } => crate::binding::bootstrap_ingress_tls(&binding_ref, install_system_trust, yes, json),
        crate::BindingCommands::ServeIngress {
            binding_ref,
            manifest,
            upstream_url,
        } => {
            crate::binding::serve_ingress_binding(&binding_ref, &manifest, upstream_url.as_deref())
        }
        crate::BindingCommands::RegisterIngress {
            manifest,
            service_name,
            url,
            json,
        } => crate::binding::register_ingress_binding_from_manifest(
            &manifest,
            &service_name,
            &url,
            json,
        ),
        crate::BindingCommands::RegisterService {
            manifest,
            service_name,
            url,
            process_id,
            port,
            json,
        } => match (url.as_deref(), process_id.as_deref()) {
            (Some(url), _) => crate::binding::register_service_binding_from_manifest(
                &manifest,
                &service_name,
                url,
                json,
            ),
            (None, Some(process_id)) => crate::binding::register_service_binding_from_process(
                process_id,
                &service_name,
                port,
                json,
            ),
            (None, None) => anyhow::bail!("register-service requires either --url or --process-id"),
        },
        crate::BindingCommands::SyncProcess { process_id, json } => {
            crate::binding::sync_service_bindings_from_process(&process_id, json)
        }
    }
}
