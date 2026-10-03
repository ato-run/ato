//! Pure physical process bindings shared by Runtime lowering and registry
//! projection. These paths are realization facts, never Capsule identity.
use std::collections::BTreeMap;

pub const TOOLCHAIN_ROOT: &str = "/opt/ato/toolchains";
pub const SYSTEM_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
pub fn python_home(version: &str) -> String {
    format!("{TOOLCHAIN_ROOT}/python/{version}")
}
pub fn node_home(version: &str) -> String {
    format!("{TOOLCHAIN_ROOT}/node/{version}")
}
pub fn package_manager_home(name: &str, version: &str) -> String {
    format!("{TOOLCHAIN_ROOT}/{name}/{version}")
}
pub fn toolchain_bin_dirs(
    runtime: &BTreeMap<String, String>,
    manager: Option<(&str, &str)>,
) -> Vec<String> {
    let mut dirs = Vec::new();
    if let Some((name, version)) = manager {
        dirs.push(format!("{}/bin", package_manager_home(name, version)));
    }
    if let Some(version) = runtime.get("node") {
        dirs.push(format!("{}/bin", node_home(version)));
    }
    if let Some(version) = runtime.get("python") {
        dirs.push(format!("{}/bin", python_home(version)));
    }
    dirs
}
pub fn environment(
    python: bool,
    toolchains: &BTreeMap<String, String>,
    manager: Option<(&str, &str)>,
    guest_root: &str,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    if python {
        if let Some(version) = toolchains.get("python") {
            let minor = version.rsplit_once('.').map(|(m, _)| m).unwrap_or(version);
            env.insert(
                "PYTHONPATH".into(),
                format!(
                    "{}/.venv/lib/python{minor}/site-packages",
                    guest_root.trim_end_matches('/')
                ),
            );
        }
    } else {
        env.insert(
            "PATH".into(),
            toolchain_bin_dirs(toolchains, manager)
                .into_iter()
                .chain([SYSTEM_PATH.into()])
                .collect::<Vec<_>>()
                .join(":"),
        );
        env.insert("HOME".into(), "/tmp".into());
        if toolchains.contains_key("node") {
            env.insert("npm_config_cache".into(), "/tmp/.npm".into());
            env.insert("npm_config_update_notifier".into(), "false".into());
        }
    }
    env
}
pub fn workspace_relative_cwd(cwd: &str) -> Result<String, String> {
    if cwd.contains('\0') || cwd.contains('\\') {
        return Err(format!("cwd {cwd:?} is not a workspace path"));
    }
    if cwd.starts_with('/') {
        return Err(format!(
            "cwd {cwd:?} is absolute; a step runs inside the workspace"
        ));
    }
    let mut parts = Vec::new();
    for part in cwd.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                return Err(format!(
                    "cwd {cwd:?} climbs with `..`; a step runs inside the workspace"
                ));
            }
            name => parts.push(name),
        }
    }
    Ok(parts.join("/"))
}
