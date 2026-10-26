use super::*;
use crate::launch::resolved::ResolvedReadOnlyInput;
use ato_ipc::runtime_launch_v2::RuntimeLaunchSpecV2;
use std::path::Path;

fn group() -> OciServiceGroupV2 {
    RuntimeLaunchSpecV2::parse(include_str!(
        "../../../../ipc/tests/fixtures/runtime-launch-spec-v2/service-group.json"
    ))
    .unwrap()
    .service_group()
    .clone()
}
fn context(root: &Path) -> ResolvedRuntimeLaunchContext {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    ResolvedRuntimeLaunchContext::new(workspace, "", BTreeMap::new(), vec![], vec![], vec![])
        .unwrap()
}
fn model(root: &Path, id: &str) -> ResolvedReadOnlyInput {
    let path = root.join(id);
    std::fs::create_dir_all(&path).unwrap();
    ResolvedReadOnlyInput {
        input_id: Some(id.into()),
        env_name: input_env_name(id),
        path,
    }
}

#[test]
fn explicit_consumers_only_receive_their_readonly_inputs_and_independent_outputs() {
    let root = crate::launch::test_temp_dir();
    let mut group = group();
    group.services[0].input_ids = Some(vec!["models".into()]);
    let input = model(root.path(), "models");
    let outputs = root.path().join("outputs");
    let context = context(root.path())
        .with_read_only_inputs(vec![input.clone()])
        .with_output_dir(Some(outputs.clone()))
        .with_data_grant_present(true);
    let mut mounts = vec![];
    let mut environment = BTreeMap::new();
    attach(
        &group,
        &group.services[0],
        &context,
        &mut mounts,
        &mut environment,
    )
    .unwrap();
    assert_eq!(mounts.len(), 2);
    assert_eq!(mounts[0].host_path, input.path);
    assert!(!mounts[0].writable);
    assert_eq!(mounts[1].host_path, outputs.join("backend"));
    assert!(mounts[1].writable);
    assert_eq!(environment["ATO_INPUT_PATH_MODELS"], "/.ato-data/inputs/0");
    mounts.clear();
    environment.clear();
    attach(
        &group,
        &group.services[1],
        &context,
        &mut mounts,
        &mut environment,
    )
    .unwrap();
    assert!(mounts.is_empty() && environment.is_empty());
    assert!(!outputs.join("web").exists());
    // [] is an explicit output-only mapping, separate from omitted None.
    group.services[1].input_ids = Some(vec![]);
    attach(
        &group,
        &group.services[1],
        &context,
        &mut mounts,
        &mut environment,
    )
    .unwrap();
    assert_eq!(mounts.len(), 1);
    assert_eq!(mounts[0].host_path, outputs.join("web"));
    assert!(!environment.contains_key("ATO_INPUT_PATH_MODELS"));
    assert_ne!(mounts[0].host_path, outputs.join("backend"));
    // A second explicit consumer may share the same read-only tree.
    group.services[1].input_ids = Some(vec!["models".into()]);
    mounts.clear();
    environment.clear();
    attach(
        &group,
        &group.services[1],
        &context,
        &mut mounts,
        &mut environment,
    )
    .unwrap();
    assert_eq!(mounts[0].host_path, input.path);
    assert!(!mounts[0].writable);
}

#[test]
fn no_grant_unassigned_missing_or_colliding_inputs_fail_before_output_creation() {
    let root = crate::launch::test_temp_dir();
    let mut group = group();
    group.services[0].input_ids = Some(vec!["models".into()]);
    let input = model(root.path(), "models");
    let outputs = root.path().join("outputs");
    let make = |inputs, grant| {
        context(root.path())
            .with_read_only_inputs(inputs)
            .with_output_dir(Some(outputs.clone()))
            .with_data_grant_present(grant)
    };
    let check = |g: &OciServiceGroupV2, ctx: &ResolvedRuntimeLaunchContext| {
        assert!(attach(g, &g.services[0], ctx, &mut vec![], &mut BTreeMap::new()).is_err());
        assert!(!outputs.exists());
    };
    check(&group, &make(vec![input.clone()], false));
    check(&group, &make(vec![], true));
    check(
        &group,
        &make(vec![input.clone(), model(root.path(), "assets")], true),
    );
    check(&group, &make(vec![input.clone(), input.clone()], true));
    let mut inconsistent = input.clone();
    inconsistent.input_id = None;
    check(&group, &make(vec![inconsistent], true));
    let mut inconsistent = input.clone();
    inconsistent.env_name = "ATO_INPUT_PATH_OTHER".into();
    check(&group, &make(vec![inconsistent], true));
    group.services[0].input_ids = Some(vec!["a-b".into()]);
    group.services[1].input_ids = Some(vec!["a.b".into()]);
    check(
        &group,
        &make(
            vec![model(root.path(), "a-b"), model(root.path(), "a.b")],
            true,
        ),
    );
}

#[test]
fn legacy_none_cannot_ignore_any_grant_and_empty_assets_needs_actual_delivery() {
    let root = crate::launch::test_temp_dir();
    let mut group = group();
    assert!(validate(&group, &context(root.path())).is_ok());
    assert!(validate(&group, &context(root.path()).with_data_grant_present(true)).is_err());
    group.services[0].input_ids = Some(vec!["assets".into()]);
    let ctx = context(root.path()).with_data_grant_present(true);
    assert!(validate(&group, &ctx).is_err());
    let assets = model(root.path(), "assets");
    assert_eq!(assets.env_name, "ATO_INPUT_ASSETS_DIR");
    let ctx = ctx.with_read_only_inputs(vec![assets]);
    let mut mounts = vec![];
    attach(
        &group,
        &group.services[0],
        &ctx,
        &mut mounts,
        &mut BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(mounts.len(), 1);
    assert!(!mounts[0].writable);
}

#[cfg(unix)]
#[test]
fn output_symlink_and_input_overlap_are_rejected_without_removing_foreign_files() {
    let root = crate::launch::test_temp_dir();
    let mut group = group();
    group.services[0].input_ids = Some(vec!["models".into()]);
    let input = model(root.path(), "models");
    std::fs::write(input.path.join("canary"), b"keep").unwrap();
    let outputs = root.path().join("outputs");
    std::os::unix::fs::symlink(&input.path, &outputs).unwrap();
    let ctx = context(root.path())
        .with_read_only_inputs(vec![input.clone()])
        .with_data_grant_present(true)
        .with_output_dir(Some(outputs));
    assert!(
        attach(
            &group,
            &group.services[0],
            &ctx,
            &mut vec![],
            &mut BTreeMap::new()
        )
        .is_err()
    );
    assert_eq!(std::fs::read(input.path.join("canary")).unwrap(), b"keep");
    let ctx = ctx.with_output_dir(Some(root.path().to_path_buf()));
    assert!(validate(&group, &ctx).is_err());
}

#[cfg(target_os = "linux")]
mod linux;
