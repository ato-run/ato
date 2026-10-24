//! The final executable gate a process launch passes before it spawns.
//!
//! The admission gates classify what a Derivation *declares*. This gate
//! classifies what the launch would actually exec: the resolved `argv[0]`, its
//! canonical (symlink-resolved) target, and every environment value it would
//! inherit, with `PATH` first among equals. A Derivation can declare a patched
//! Node and still name an affected toolchain in `argv` or an authored
//! `env.PATH`; that is a policy bypass only this layer closes.
//!
//! It lives next to the launcher, not in the policy table: the table answers
//! "is this release patched?", and this module answers "which release would
//! this spawn reach?".
//!
//! ## What this does and does not close
//!
//! Closed: an absolute toolchain path in `argv`, a `PATH` entry that names one,
//! a symlink whose target is one, and a bare `node` that resolves outside a
//! patched toolchain (unverifiable, fail closed).
//!
//! Not closed by this layer: a child process that later execs an absolute
//! toolchain path it discovered itself. That is undecidable before the spawn;
//! the mount restriction (`launch::sandbox`) is what removes the other Node
//! versions from the filesystem in namespace launches.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use ato_formation::authoring::BoundDerivation;
use ato_formation::runtime_policy::{self, RuntimePolicyRefusal};

/// The absolute root every provisioned toolchain lives under.
const TOOLCHAIN_ROOT_PREFIX: &str = "/opt/ato/toolchains/";

/// What the spawner would exec, once resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedExecutable {
    /// The canonical path when the file exists, else the lexical candidate.
    pub path: PathBuf,
    /// The Node release named by the path (canonical or lexical), if the path
    /// is under a `/opt/ato/toolchains/node/<release>` directory.
    pub node_version: Option<String>,
}

impl ValidatedExecutable {
    pub fn path_text(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

fn policy_error(refusal: RuntimePolicyRefusal) -> anyhow::Error {
    anyhow::anyhow!("{}: {}", refusal.code, refusal.message)
}

fn is_node_name(program: &str) -> bool {
    Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, "node" | "nodejs"))
}

fn search(program: &str, path_value: &str) -> Option<PathBuf> {
    path_value
        .split(':')
        .filter(|directory| !directory.is_empty())
        .map(|directory| Path::new(directory).join(program))
        .find(|candidate| candidate.is_file())
}

fn inherited_path() -> String {
    std::env::var("PATH").unwrap_or_default()
}

/// The path a spawner would try for `program`: absolute as-is, a relative path
/// containing `/` against the workload cwd, and a bare name against `PATH`
/// (the launch environment's, or the Runner's when the spec states none, which
/// is what the child would inherit).
fn lexical_path(program: &str, cwd: &Path, path_value: &str) -> PathBuf {
    let path = Path::new(program);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    if program.contains('/') {
        return cwd.join(path);
    }
    let value = if path_value.is_empty() {
        inherited_path()
    } else {
        path_value.to_owned()
    };
    search(program, &value).unwrap_or_else(|| PathBuf::from(program))
}

/// Resolve the executable and apply the runtime policy to everything the spawn
/// would see.
pub fn validate_and_resolve(
    program: &str,
    cwd: &Path,
    environment: &BTreeMap<String, String>,
    node_implied: bool,
) -> Result<ValidatedExecutable> {
    runtime_policy::environment_runtime_policy(environment).map_err(policy_error)?;
    let path_value = environment.get("PATH").map(String::as_str).unwrap_or("");
    let lexical = lexical_path(program, cwd, path_value);
    let canonical = std::fs::canonicalize(&lexical).ok();
    runtime_policy::executable_runtime_policy(&lexical, canonical.as_deref(), node_implied)
        .map_err(policy_error)?;
    let path = canonical.clone().unwrap_or_else(|| lexical.clone());
    let node_version = runtime_policy::node_toolchain_version(&path.to_string_lossy())
        .map(str::to_owned)
        .or_else(|| {
            runtime_policy::node_toolchain_version(&lexical.to_string_lossy()).map(str::to_owned)
        });
    Ok(ValidatedExecutable { path, node_version })
}

/// The same gate for a sealed Derivation, whose declaration decides whether a
/// `node`-named executable is expected.
pub fn validate_derivation_executable(
    program: &str,
    cwd: &Path,
    environment: &BTreeMap<String, String>,
    derivation: &BoundDerivation,
) -> Result<ValidatedExecutable> {
    let node_implied =
        runtime_policy::derivation_declares_node_runtime(derivation) || is_node_name(program);
    validate_and_resolve(program, cwd, environment, node_implied)
}

/// A toolchain a launch needs at its guest path.
///
/// The resolved launch environment names them: Formation's plan built `PATH`
/// from `plan.toolchain_path`, and the launch spec carries that same value, so
/// the PATH entries under `/opt/ato/toolchains/<name>/<release>` are the
/// resolved form of the plan's `bin` directories. The resolved executable's own
/// toolchain is included even if `PATH` omitted it.
pub fn toolchain_roots(
    environment: &BTreeMap<String, String>,
    resolved: Option<&Path>,
) -> Vec<PathBuf> {
    let mut roots = BTreeSet::new();
    for value in environment.values() {
        roots_in(value, &mut roots);
    }
    if let Some(resolved) = resolved {
        roots_in(&resolved.to_string_lossy(), &mut roots);
    }
    roots.into_iter().collect()
}

fn roots_in(value: &str, roots: &mut BTreeSet<PathBuf>) {
    for (index, _) in value.match_indices(TOOLCHAIN_ROOT_PREFIX) {
        let after = &value[index + TOOLCHAIN_ROOT_PREFIX.len()..];
        let mut segments = after
            .split(|character: char| {
                character == ':'
                    || character.is_whitespace()
                    || matches!(character, ';' | '"' | '\'')
            })
            .next()
            .unwrap_or_default()
            .split('/');
        let Some(name) = segments.next().filter(|name| valid_segment(name)) else {
            continue;
        };
        let Some(release) = segments.next().filter(|release| valid_segment(release)) else {
            continue;
        };
        roots.insert(Path::new(TOOLCHAIN_ROOT_PREFIX).join(name).join(release));
    }
}

fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment.len() <= 64
        && segment.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | '+')
        })
}

/// The executable gate as an admission-shaped failure message.
pub fn context_for_launch() -> &'static str {
    "the process launch was refused by the runtime policy"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(values: &[(&str, &str)]) -> BTreeMap<String, String> {
        values
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn an_absolute_affected_toolchain_in_argv_is_refused() {
        let cwd = Path::new("/app");
        let error = validate_and_resolve(
            "/opt/ato/toolchains/node/22.14.0/bin/node",
            cwd,
            &environment(&[("PATH", "/usr/local/bin:/usr/bin:/bin")]),
            true,
        )
        .expect_err("affected absolute argv");
        assert!(
            format!("{error:#}").contains("runtime_version_not_patched"),
            "{error:#}"
        );
    }

    #[test]
    fn a_path_entry_naming_an_affected_toolchain_is_refused() {
        let workspace = tempfile::tempdir().unwrap();
        let bin = workspace.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let node = bin.join("node");
        std::fs::write(&node, b"#!/bin/sh\n").unwrap();
        let path = format!(
            "/opt/ato/toolchains/node/22.14.0/bin:{}:/usr/bin:/bin",
            bin.display()
        );
        let error = validate_and_resolve(
            "node",
            workspace.path(),
            &environment(&[("PATH", &path)]),
            true,
        )
        .expect_err("affected PATH entry");
        assert!(
            format!("{error:#}").contains("runtime_version_not_patched"),
            "{error:#}"
        );
    }

    #[test]
    fn a_bare_node_outside_the_toolchain_is_unverifiable_not_patched() {
        let workspace = tempfile::tempdir().unwrap();
        let bin = workspace.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("node"), b"#!/bin/sh\n").unwrap();
        let path = format!("{}:/usr/bin:/bin", bin.display());
        let error = validate_and_resolve(
            "node",
            workspace.path(),
            &environment(&[("PATH", &path)]),
            true,
        )
        .expect_err("system node");
        assert!(
            format!("{error:#}").contains("runtime_version_unverifiable"),
            "{error:#}"
        );
    }

    #[test]
    fn a_missing_bare_node_is_fail_closed() {
        let workspace = tempfile::tempdir().unwrap();
        let error = validate_and_resolve(
            "node",
            workspace.path(),
            &environment(&[("PATH", workspace.path().to_str().unwrap())]),
            true,
        )
        .expect_err("missing node");
        assert!(
            format!("{error:#}").contains("runtime_version_unverifiable"),
            "{error:#}"
        );
    }

    #[test]
    fn a_symlink_to_an_affected_toolchain_is_classified_by_its_target() {
        // The helper is what the canonicalized path feeds; a symlink whose
        // lexical name is innocent and whose target is affected is refused.
        let refusal = runtime_policy::executable_runtime_policy(
            Path::new("/usr/local/bin/node"),
            Some(Path::new("/opt/ato/toolchains/node/22.14.0/bin/node")),
            true,
        )
        .expect_err("symlink target");
        assert_eq!(refusal.code, runtime_policy::RUNTIME_VERSION_NOT_PATCHED);
    }

    #[test]
    fn a_patched_toolchain_outside_the_declaration_is_allowed() {
        assert!(
            runtime_policy::executable_runtime_policy(
                Path::new("/opt/ato/toolchains/node/22.23.4/bin/node"),
                Some(Path::new("/opt/ato/toolchains/node/22.23.4/bin/node")),
                true,
            )
            .is_ok()
        );
    }

    #[test]
    fn a_route_without_node_is_unchanged() {
        let workspace = tempfile::tempdir().unwrap();
        let bin = workspace.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("python3"), b"#!/bin/sh\n").unwrap();
        let path = format!("{}:/usr/bin:/bin", bin.display());
        let validated = validate_and_resolve(
            "python3",
            workspace.path(),
            &environment(&[("PATH", &path)]),
            false,
        )
        .expect("python is not this policy's business");
        assert_eq!(validated.node_version, None);
    }

    #[test]
    fn the_needed_toolchains_are_derived_from_the_launch_environment() {
        let env = environment(&[
            (
                "PATH",
                "/opt/ato/toolchains/node/22.23.4/bin:/opt/ato/toolchains/python/3.12.7/bin:/usr/bin",
            ),
            ("NODE_HOME", "/opt/ato/toolchains/node/22.23.4"),
        ]);
        let roots = toolchain_roots(&env, None);
        assert_eq!(
            roots,
            vec![
                PathBuf::from("/opt/ato/toolchains/node/22.23.4"),
                PathBuf::from("/opt/ato/toolchains/python/3.12.7"),
            ]
        );
        // Nothing is derived from an environment that names no toolchain, so a
        // browser-only evaluation mounts nothing.
        assert!(toolchain_roots(&environment(&[("PATH", "/usr/bin:/bin")]), None).is_empty());
    }
}
