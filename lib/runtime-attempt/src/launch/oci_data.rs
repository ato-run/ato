//! Physical delivery of a Run's already-authorized files into one container.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result, ensure};
use ato_adapter_oci::OciMount;

use super::resolved::ResolvedRuntimeLaunchContext;

const GUEST_ROOT: &str = "/.ato-data";

fn directory(path: &Path) -> Result<()> {
    ensure!(path.is_absolute(), "OCI data path must be absolute");
    for part in path.ancestors() {
        let metadata = std::fs::symlink_metadata(part).context("inspect OCI data directory")?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "OCI data directory must not contain symlinks"
        );
    }
    ensure!(
        path.canonicalize()? == path,
        "OCI data directory must be normalized"
    );
    Ok(())
}

fn overlaps(path: &str) -> bool {
    let path = Path::new(path);
    path.starts_with(GUEST_ROOT) || Path::new(GUEST_ROOT).starts_with(path)
}

pub(super) fn attach(
    context: &ResolvedRuntimeLaunchContext,
    workspace_target: &str,
    mounts: &mut Vec<OciMount>,
    environment: &mut BTreeMap<String, String>,
) -> Result<()> {
    if context.read_only_inputs().is_empty() && context.output_dir().is_none() {
        return Ok(());
    }
    ensure!(
        !overlaps(workspace_target) && mounts.iter().all(|m| !overlaps(&m.guest_path)),
        "OCI data mount conflicts with workspace or state"
    );
    let mut names = BTreeSet::new();
    for input in context.read_only_inputs() {
        ensure!(
            (input.env_name == "ATO_INPUT_ASSETS_DIR"
                || input
                    .env_name
                    .strip_prefix("ATO_INPUT_PATH_")
                    .is_some_and(|id| !id.is_empty()
                        && id
                            .bytes()
                            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')))
                && names.insert(input.env_name.as_str())
                && !environment.contains_key(&input.env_name),
            "OCI data input name is unsafe, duplicated or already bound"
        );
        directory(&input.path)?;
    }
    if let Some(output) = context.output_dir() {
        ensure!(
            !environment.contains_key("ATO_OUTPUT_DIR"),
            "OCI data output is already bound"
        );
        ensure!(
            context
                .read_only_inputs()
                .iter()
                .all(|i| !i.path.starts_with(output) && !output.starts_with(&i.path))
                && !output.starts_with(context.workspace_root())
                && !context.workspace_root().starts_with(output)
                && context
                    .state_attachments()
                    .iter()
                    .all(|s| !output.starts_with(s.working_copy_for_mount())
                        && !s.working_copy_for_mount().starts_with(output)),
            "OCI data output overlaps another input or state"
        );
        match std::fs::symlink_metadata(output) {
            Ok(_) => directory(output)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                directory(output.parent().context("OCI data output has no parent")?)?;
                std::fs::create_dir(output).context("create OCI data output directory")?;
                directory(output)?;
            }
            Err(error) => return Err(error.into()),
        }
        if let Some(boundary) = super::host_boundary::active() {
            super::host_boundary::prepare_output_ownership(output, boundary)?;
        }
    }
    for (index, input) in context.read_only_inputs().iter().enumerate() {
        let target = format!("{GUEST_ROOT}/inputs/{index}");
        environment.insert(input.env_name.clone(), target.clone());
        mounts.push(OciMount {
            host_path: input.path.clone(),
            guest_path: target,
            writable: false,
        });
    }
    if let Some(output) = context.output_dir() {
        let target = format!("{GUEST_ROOT}/outputs");
        environment.insert("ATO_OUTPUT_DIR".into(), target.clone());
        mounts.push(OciMount {
            host_path: output.to_path_buf(),
            guest_path: target,
            writable: true,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::resolved::ResolvedReadOnlyInput;

    fn context(root: &Path) -> ResolvedRuntimeLaunchContext {
        ResolvedRuntimeLaunchContext::new(
            root.to_path_buf(),
            "",
            BTreeMap::new(),
            vec![],
            vec![],
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn authorized_inputs_and_output_are_guest_paths_with_independent_access() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let assets = root.path().join("assets");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&assets).unwrap();
        let output = root.path().join("outputs");
        let context = context(&workspace)
            .with_read_only_inputs(vec![ResolvedReadOnlyInput {
                input_id: None,
                env_name: "ATO_INPUT_ASSETS_DIR".into(),
                path: assets.clone(),
            }])
            .with_output_dir(Some(output.clone()));
        let mut mounts = vec![];
        let mut env = BTreeMap::new();
        attach(&context, "/app", &mut mounts, &mut env).unwrap();
        assert_eq!(env["ATO_INPUT_ASSETS_DIR"], "/.ato-data/inputs/0");
        assert_eq!(env["ATO_OUTPUT_DIR"], "/.ato-data/outputs");
        assert!(output.is_dir());
        assert_eq!(mounts[0].host_path, assets);
        assert!(!mounts[0].writable);
        assert_eq!(mounts[1].host_path, output);
        assert!(mounts[1].writable);
        assert!(
            env.values()
                .all(|v| !v.contains(root.path().to_str().unwrap()))
        );
    }

    #[test]
    fn overlapping_guest_workspace_or_state_and_environment_are_refused() {
        let root = tempfile::tempdir().unwrap();
        let context = context(root.path()).with_read_only_inputs(vec![ResolvedReadOnlyInput {
            input_id: None,
            env_name: "ATO_INPUT_ASSETS_DIR".into(),
            path: root.path().to_path_buf(),
        }]);
        for workspace in ["/.ato-data", "/.ato-data/app", "/"] {
            assert!(attach(&context, workspace, &mut vec![], &mut BTreeMap::new()).is_err());
        }
        let mut mounts = vec![OciMount {
            host_path: root.path().to_path_buf(),
            guest_path: "/.ato-data/state".into(),
            writable: true,
        }];
        assert!(attach(&context, "/app", &mut mounts, &mut BTreeMap::new()).is_err());
        let mut env = BTreeMap::from([("ATO_INPUT_ASSETS_DIR".into(), "authored".into())]);
        assert!(attach(&context, "/app", &mut vec![], &mut env).is_err());
    }

    #[test]
    fn input_names_and_output_overlap_are_refused_before_creation() {
        let root = tempfile::tempdir().unwrap();
        for name in ["HOME", "ATO_INPUT_PATH_", "ATO_INPUT_PATH_lowercase"] {
            let c = context(root.path()).with_read_only_inputs(vec![ResolvedReadOnlyInput {
                input_id: None,
                env_name: name.into(),
                path: root.path().to_path_buf(),
            }]);
            assert!(attach(&c, "/app", &mut vec![], &mut BTreeMap::new()).is_err());
        }
        let output = root.path().join("outputs");
        let c = context(root.path()).with_output_dir(Some(output.clone()));
        assert!(attach(&c, "/app", &mut vec![], &mut BTreeMap::new()).is_err());
        assert!(!output.exists());
        let duplicate = ResolvedReadOnlyInput {
            input_id: None,
            env_name: "ATO_INPUT_ASSETS_DIR".into(),
            path: root.path().to_path_buf(),
        };
        let c = context(root.path()).with_read_only_inputs(vec![duplicate.clone(), duplicate]);
        assert!(attach(&c, "/app", &mut vec![], &mut BTreeMap::new()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_input_and_output_ancestors_are_refused() {
        let root = tempfile::tempdir().unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(root.path(), &link).unwrap();
        let c = context(root.path()).with_read_only_inputs(vec![ResolvedReadOnlyInput {
            input_id: None,
            env_name: "ATO_INPUT_ASSETS_DIR".into(),
            path: link.clone(),
        }]);
        assert!(attach(&c, "/app", &mut vec![], &mut BTreeMap::new()).is_err());
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let c = context(&workspace).with_output_dir(Some(link.join("outputs")));
        assert!(attach(&c, "/app", &mut vec![], &mut BTreeMap::new()).is_err());
        assert!(!root.path().join("outputs").exists());
    }
}
