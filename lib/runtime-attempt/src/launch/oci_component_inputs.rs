//! Physical per-component delivery of the existing lease-bound data Grant.
use super::resolved::ResolvedRuntimeLaunchContext;
use anyhow::{Context, Result, ensure};
use ato_adapter_oci::OciMount;
use ato_ipc::{
    oci_component_inputs::{input_env_name, validate_input_ids},
    oci_service_group::is_service_name,
    runtime_launch_v2::{OciServiceGroupV2, OciServiceV2},
};
use std::collections::{BTreeMap, BTreeSet};

fn validate(group: &OciServiceGroupV2, context: &ResolvedRuntimeLaunchContext) -> Result<()> {
    let explicit = group
        .services
        .iter()
        .any(|service| service.input_ids.is_some());
    if !explicit {
        ensure!(
            context.read_only_inputs().is_empty()
                && context.output_dir().is_none()
                && !context.data_grant_present(),
            "oci_group_data_plane_unsupported: explicit component assignment is required"
        );
        return Ok(());
    }
    ensure!(
        context.data_grant_present(),
        "component data assignment has no lease-bound Grant"
    );
    if let Some(output) = context.output_dir() {
        ensure!(
            context
                .read_only_inputs()
                .iter()
                .all(|input| !input.path.starts_with(output) && !output.starts_with(&input.path)),
            "component output overlaps a delivered input"
        );
    }
    let mut selected = BTreeSet::new();
    let mut projected = BTreeMap::new();
    for service in &group.services {
        ensure!(
            is_service_name(&service.name),
            "unsafe component output name"
        );
        if let Some(ids) = &service.input_ids {
            validate_input_ids(ids).map_err(anyhow::Error::msg)?;
            for id in ids {
                let name = input_env_name(id);
                ensure!(
                    projected.get(&name).is_none_or(|previous| *previous == id),
                    "component input environment collision"
                );
                projected.insert(name, id);
                selected.insert(id.as_str());
            }
        }
    }
    let mut delivered = BTreeSet::new();
    let mut names = BTreeSet::new();
    for input in context.read_only_inputs() {
        let id = input
            .input_id
            .as_deref()
            .context("component input lost its logical Grant ID")?;
        ensure!(
            selected.contains(id)
                && delivered.insert(id)
                && names.insert(input.env_name.as_str())
                && input.env_name == input_env_name(id),
            "unassigned, duplicated or inconsistent component input"
        );
    }
    ensure!(
        selected == delivered,
        "component input selector was not delivered by the Grant"
    );
    Ok(())
}

pub(super) fn attach(
    group: &OciServiceGroupV2,
    service: &OciServiceV2,
    context: &ResolvedRuntimeLaunchContext,
    mounts: &mut Vec<OciMount>,
    environment: &mut BTreeMap<String, String>,
) -> Result<()> {
    validate(group, context)?;
    let Some(ids) = &service.input_ids else {
        return Ok(());
    };
    if context.output_dir().is_some()
        && let Some(boundary) = super::host_boundary::active()
    {
        ensure!(
            service
                .user
                .is_some_and(|user| user.uid == boundary.uid && user.gid == boundary.gid),
            "oci_user_state_owner_mismatch: component output requires the configured workload identity"
        );
    }
    let selected = ids.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let inputs = context
        .read_only_inputs()
        .iter()
        .filter(|input| {
            input
                .input_id
                .as_deref()
                .is_some_and(|id| selected.contains(id))
        })
        .cloned()
        .collect();
    let output = context.output_dir().map(|root| root.join(&service.name));
    if let Some(root) = context.output_dir() {
        // Reuse the existing directory/overlap/ownership validator to prepare
        // only this Run's output parent. The temporary mount list is not spawned.
        super::oci_data::attach(
            &context.component_data_view(vec![], Some(root.to_path_buf())),
            &service.workspace_mount_path,
            &mut vec![],
            &mut BTreeMap::new(),
        )?;
    }
    let scoped = context.component_data_view(inputs, output);
    super::oci_data::attach(&scoped, &service.workspace_mount_path, mounts, environment)
}

#[cfg(test)]
mod tests;
