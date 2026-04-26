//! Ato Userland — 4th state layer for imperative `npm install -g` etc. in the
//! REPL.
//!
//! Design: see `docs/rfcs/draft/` (userland layer) and the session design doc.
//! In short, we intercept toolchain spawns and:
//!
//! 1. Redirect install destinations via env (`NPM_CONFIG_PREFIX`, `PIP_PREFIX`,
//!    `DENO_INSTALL_ROOT`, `UV_TOOL_DIR`, ...) into `~/.ato/userland/<family>/`.
//! 2. Expose `~/.ato/userland/<family>/bin/` as a second resolution step after
//!    `~/.ato/toolchains/` so installed binaries (`claude`, `tsc`, ...) are
//!    usable on the next REPL line.
//! 3. Auto-allow well-known package-registry hosts on install-verb detection.
//!
//! The module is intentionally UI-free: it only computes paths and env
//! bindings. Wiring into the REPL is done by `orchestrator::spawn_ato_run_repl`.
//!
//! All state lives under `~/.ato/userland/` and never leaks to the host OS.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Toolchain family (i.e. the ecosystem the binary is part of).
///
/// We classify by the argv[0] of the toolchain, not by installed package name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Node,
    Python,
    Deno,
}

impl Family {
    /// Classify `argv[0]` into a known family, or `None` for unknown binaries.
    ///
    /// Known binaries: `npm`, `npx`, `pnpm`, `yarn`, `node` → Node; `pip`,
    /// `pip3`, `python`, `python3`, `uv`, `uvx` → Python; `deno` → Deno.
    pub fn classify(argv0: &str) -> Option<Self> {
        match argv0 {
            "npm" | "npx" | "pnpm" | "yarn" | "node" => Some(Family::Node),
            "pip" | "pip3" | "python" | "python3" | "uv" | "uvx" => Some(Family::Python),
            "deno" => Some(Family::Deno),
            _ => None,
        }
    }

    fn dir_name(self) -> &'static str {
        match self {
            Family::Node => "node",
            Family::Python => "python",
            Family::Deno => "deno",
        }
    }
}

/// Resolve the userland root directory for a family, creating it lazily.
///
/// Returns `None` if `$HOME` is unavailable (should never happen on macOS/Linux
/// in practice, but we do not panic).
pub fn family_root(family: Family) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".ato").join("userland").join(family.dir_name()))
}

/// Env vars to inject when spawning a toolchain binary so that global-install
/// destinations land under `~/.ato/userland/<family>/` instead of system paths.
///
/// Applies to both install and non-install commands — the redirect is always
/// in effect so e.g. `npm ls -g` sees the userland prefix.
pub fn install_env(family: Family) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    let Some(root) = family_root(family) else {
        return env;
    };
    let root_s = root.to_string_lossy().to_string();

    match family {
        Family::Node => {
            // npm reads NPM_CONFIG_PREFIX; pnpm reads PNPM_HOME. yarn honors
            // npm_config_prefix too. Setting both is safe: they all point at
            // the same directory.
            env.insert("NPM_CONFIG_PREFIX".to_string(), root_s.clone());
            env.insert("PNPM_HOME".to_string(), root_s.clone());
            // Make installed binaries' `require()` find their siblings.
            let node_modules = root.join("lib").join("node_modules");
            env.insert(
                "NODE_PATH".to_string(),
                node_modules.to_string_lossy().to_string(),
            );
        }
        Family::Python => {
            // PYTHONUSERBASE makes `pip install --user` land under
            // <root>/lib/pythonX.Y/site-packages. PIP_PREFIX is a hard
            // redirect used by plain `pip install` (no --user) for people
            // typing what they know.
            env.insert("PYTHONUSERBASE".to_string(), root_s.clone());
            env.insert("PIP_PREFIX".to_string(), root_s.clone());
            // `uv tool install` honors UV_TOOL_DIR + UV_TOOL_BIN_DIR; give it
            // its own subdir so uv-managed tools stay separable from pip.
            let uv_tools = root.join("uv-tools");
            env.insert(
                "UV_TOOL_DIR".to_string(),
                uv_tools.to_string_lossy().to_string(),
            );
            env.insert(
                "UV_TOOL_BIN_DIR".to_string(),
                root.join("bin").to_string_lossy().to_string(),
            );
        }
        Family::Deno => {
            env.insert("DENO_INSTALL_ROOT".to_string(), root_s.clone());
        }
    }

    env
}

/// Detect install-verb in argv so we can auto-allow package-registry hosts.
///
/// Returns the list of host patterns to add to `session_allow`, or empty if
/// this is not an install command.
///
/// We keep the allowlist tight (just the canonical registry + CDN) so that
/// an install command doesn't silently unlock arbitrary egress.
pub fn install_verb_allowlist(argv: &[String]) -> Vec<String> {
    if argv.is_empty() {
        return Vec::new();
    }
    let head = argv[0].as_str();
    // Case 1: single-verb installers — `pip install X`, `npm install X`, ...
    let second = argv.get(1).map(|s| s.as_str());
    let third = argv.get(2).map(|s| s.as_str());

    let node_hosts = || {
        vec![
            "registry.npmjs.org".to_string(),
            "registry.yarnpkg.com".to_string(),
            "*.npmjs.org".to_string(),
            // Binary deps (node-gyp downloads, prebuilt binaries) land here.
            "nodejs.org".to_string(),
            "github.com".to_string(),
            "objects.githubusercontent.com".to_string(),
        ]
    };
    let python_hosts = || {
        vec![
            "pypi.org".to_string(),
            "files.pythonhosted.org".to_string(),
            "*.pypi.org".to_string(),
        ]
    };
    let deno_hosts = || {
        vec![
            "deno.land".to_string(),
            "jsr.io".to_string(),
            "esm.sh".to_string(),
        ]
    };

    match (head, second, third) {
        ("npm", Some("install" | "i" | "add"), _) => node_hosts(),
        ("pnpm", Some("install" | "i" | "add"), _) => node_hosts(),
        ("yarn", Some("add" | "install"), _) => node_hosts(),
        ("pip" | "pip3", Some("install"), _) => python_hosts(),
        ("python" | "python3", Some("-m"), Some("pip")) => {
            // `python -m pip install ...` — only auto-allow if the 4th arg is
            // install; we don't want `python -m pip list` to touch egress.
            if argv.get(3).map(|s| s.as_str()) == Some("install") {
                python_hosts()
            } else {
                Vec::new()
            }
        }
        ("uv", Some("tool"), Some("install")) => python_hosts(),
        ("uv" | "uvx", Some("pip"), Some("install")) => python_hosts(),
        ("deno", Some("install" | "add" | "cache"), _) => deno_hosts(),
        _ => Vec::new(),
    }
}

/// Look up a bare command name against all userland `bin/` directories.
///
/// Returns the first match. Names containing path separators are rejected as
/// a defensive measure (never let `../foo` escape).
pub fn find_userland_binary(name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return None;
    }
    for family in [Family::Node, Family::Python, Family::Deno] {
        let Some(root) = family_root(family) else {
            continue;
        };
        let candidate = root.join("bin").join(name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Resolve which family a userland binary belongs to (used to reapply the env
/// envelope when invoking an installed tool like `claude`).
pub fn family_for_userland_binary(path: &Path) -> Option<Family> {
    for family in [Family::Node, Family::Python, Family::Deno] {
        let Some(root) = family_root(family) else {
            continue;
        };
        if path.starts_with(&root) {
            return Some(family);
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            return meta.is_file() && meta.permissions().mode() & 0o111 != 0;
        }
        false
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_known_toolchains() {
        assert_eq!(Family::classify("npm"), Some(Family::Node));
        assert_eq!(Family::classify("pnpm"), Some(Family::Node));
        assert_eq!(Family::classify("node"), Some(Family::Node));
        assert_eq!(Family::classify("pip"), Some(Family::Python));
        assert_eq!(Family::classify("python3"), Some(Family::Python));
        assert_eq!(Family::classify("uv"), Some(Family::Python));
        assert_eq!(Family::classify("deno"), Some(Family::Deno));
        assert_eq!(Family::classify("claude"), None);
        assert_eq!(Family::classify(""), None);
    }

    #[test]
    fn install_env_has_prefix_for_node() {
        let env = install_env(Family::Node);
        // Must redirect both npm and pnpm globals.
        assert!(env.contains_key("NPM_CONFIG_PREFIX"));
        assert!(env.contains_key("PNPM_HOME"));
        assert!(env.contains_key("NODE_PATH"));
        let prefix = env.get("NPM_CONFIG_PREFIX").unwrap();
        assert!(
            prefix.ends_with("/userland/node"),
            "prefix should end with /userland/node, got {prefix}"
        );
    }

    #[test]
    fn install_env_has_prefix_for_python() {
        let env = install_env(Family::Python);
        assert!(env.contains_key("PYTHONUSERBASE"));
        assert!(env.contains_key("PIP_PREFIX"));
        assert!(env.contains_key("UV_TOOL_DIR"));
        assert!(env.contains_key("UV_TOOL_BIN_DIR"));
    }

    #[test]
    fn install_verb_detects_npm_install() {
        let argv = vec![
            "npm".to_string(),
            "install".to_string(),
            "-g".to_string(),
            "@anthropic-ai/claude-code".to_string(),
        ];
        let allow = install_verb_allowlist(&argv);
        assert!(
            allow.iter().any(|h| h == "registry.npmjs.org"),
            "expected registry.npmjs.org in {allow:?}"
        );
    }

    #[test]
    fn install_verb_detects_python_m_pip_install() {
        let argv = vec![
            "python3".to_string(),
            "-m".to_string(),
            "pip".to_string(),
            "install".to_string(),
            "requests".to_string(),
        ];
        let allow = install_verb_allowlist(&argv);
        assert!(allow.iter().any(|h| h == "pypi.org"));
    }

    #[test]
    fn install_verb_ignores_non_install_commands() {
        // `npm --version` must NOT auto-allow egress — regression guard so
        // tight default policy is preserved for everyday commands.
        assert!(install_verb_allowlist(&["npm".into(), "--version".into()]).is_empty());
        assert!(
            install_verb_allowlist(&["npm".into(), "ls".into(), "-g".into()]).is_empty(),
            "`npm ls` is a query, not an install"
        );
        assert!(install_verb_allowlist(&["claude".into(), "--help".into()]).is_empty());
        // `python -m pip list` is a query, not install.
        assert!(
            install_verb_allowlist(&[
                "python".into(),
                "-m".into(),
                "pip".into(),
                "list".into()
            ])
            .is_empty()
        );
    }

    #[test]
    fn family_root_ends_with_family_name() {
        let root = family_root(Family::Node).expect("home dir available");
        assert!(root.ends_with(".ato/userland/node"), "got {}", root.display());
    }

    #[test]
    fn find_userland_binary_rejects_path_traversal() {
        assert!(find_userland_binary("../etc/passwd").is_none());
        assert!(find_userland_binary("foo/bar").is_none());
        assert!(find_userland_binary("").is_none());
    }
}
