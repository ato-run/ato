//! Platform-neutral workload visibility plan.
//!
//! Every OS backend needs to answer the same two questions before it can launch
//! a sandboxed source workload:
//!
//! 1. **Which interpreter should run?** — a project virtualenv interpreter
//!    (`<source_dir>/.venv/bin/python`) when the build phase produced one, or
//!    the managed base toolchain interpreter otherwise. A bare `python` maps to
//!    the base toolchain, which carries only the standard library; the capsule's
//!    installed dependencies live in `.venv`, so a venv-less launch fails at
//!    import time (`ModuleNotFoundError`).
//! 2. **Which host paths must the workload be able to read?** — the venv's base
//!    CPython install (a venv interpreter is a thin shim that loads stdlib and
//!    execs from that base install) and the managed toolchain's install root
//!    (`bin/` + `lib/`, so the interpreter can load `libpython*.so` / node's
//!    `lib/`).
//!
//! Historically this lived inside the Linux bubblewrap backend, so each
//! "workload can't see X" fix had to be rediscovered and re-implemented on
//! macOS (seatbelt) and Windows. [`WorkloadVisibilityPlan::compute`] derives the
//! answer once, OS-independently; each backend then *lowers* it to its own
//! mechanism (bwrap binds / seatbelt allow rules / Windows VM mapping). A
//! visibility fix lands here and applies everywhere.

use std::path::{Path, PathBuf};

use crate::launcher::SourceTarget;

/// Guest path of the venv interpreter once the source dir is mounted at `/app`
/// (the bubblewrap layout). Backends that run the workload against the host
/// filesystem (seatbelt, Windows) use [`SandboxVenv::host_python`] instead.
const GUEST_VENV_PYTHON: &str = "/app/.venv/bin/python";

/// In-sandbox interpreter selection for a project virtualenv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SandboxVenv {
    /// Host path of the venv interpreter (`<source_dir>/.venv/bin/python`, or
    /// `Scripts\python.exe` on Windows). Used by backends that exec against the
    /// host filesystem.
    pub host_python: PathBuf,
    /// Guest path of the venv interpreter for backends that remap the source
    /// dir to `/app` (bubblewrap). Always [`GUEST_VENV_PYTHON`].
    pub guest_python: String,
    /// Host path of the base CPython install the venv references, which must be
    /// visible inside the sandbox so the venv interpreter can exec and load its
    /// standard library.
    pub base_install: PathBuf,
}

/// Platform-neutral plan for what a sandboxed workload must be able to see.
///
/// Computed once via [`WorkloadVisibilityPlan::compute`] and lowered per
/// backend. Holds the chosen interpreter (when a venv applies) plus the set of
/// host paths the workload needs read access to.
#[derive(Debug, Clone, Default)]
#[cfg_attr(target_os = "windows", allow(dead_code))]
pub(super) struct WorkloadVisibilityPlan {
    /// Resolved project virtualenv interpreter, when the build phase produced a
    /// `.venv`. `None` falls back to the managed base toolchain interpreter.
    pub venv: Option<SandboxVenv>,
    /// Host paths that must be readable inside the sandbox (venv base install,
    /// toolchain install root). Order-preserving and de-duplicated. Backends
    /// lower these to their own mechanism (bwrap `--ro-bind-try`, seatbelt
    /// `allow file-read*`, …).
    pub read_paths: Vec<PathBuf>,
}

impl WorkloadVisibilityPlan {
    /// Derive the visibility plan from a launch target and the resolved managed
    /// toolchain binary.
    ///
    /// `toolchain_path` is the JIT-provisioned / local base interpreter the
    /// backend already resolved (e.g. via `ensure_toolchain`). Its install root
    /// is folded into [`Self::read_paths`] so the interpreter can load its
    /// runtime libraries.
    pub(super) fn compute(target: &SourceTarget, toolchain_path: &Path) -> Self {
        let mut read_paths: Vec<PathBuf> = Vec::new();
        let venv = sandbox_venv_python(target);
        if let Some(ref venv) = venv {
            push_unique(&mut read_paths, venv.base_install.clone());
        }
        if let Some(root) = toolchain_install_root(toolchain_path) {
            push_unique(&mut read_paths, root);
        }
        Self { venv, read_paths }
    }

    /// Host path of the project virtualenv interpreter, when one applies. Used by
    /// backends that exec the workload against the host filesystem (seatbelt,
    /// Windows). `None` falls back to the base toolchain interpreter.
    // Host-exec backends (seatbelt/Windows) only; the Linux bwrap backend reads
    // `self.venv` directly, so on Linux this accessor would be dead code.
    #[cfg(target_os = "macos")]
    pub(super) fn venv_host_python(&self) -> Option<PathBuf> {
        self.venv.as_ref().map(|v| v.host_python.clone())
    }
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.contains(&path) {
        paths.push(path);
    }
}

/// Detect a project virtualenv created by the build phase
/// (`<source_dir>/.venv`) and resolve the base CPython install it references.
///
/// A venv interpreter is a thin shim: it adds the venv's `site-packages` to
/// `sys.path` but loads the standard library (and, for symlinked venvs, execs)
/// from the base install named in `.venv/pyvenv.cfg` (`home = …/bin`). For the
/// sandboxed interpreter to work, that base install must be visible inside the
/// sandbox, so we surface its root for an additional read allowance. Returns
/// `None` when no venv interpreter exists (capsules with no third-party
/// dependencies fall back to the base toolchain interpreter) or when the base
/// install cannot be resolved.
fn sandbox_venv_python(target: &SourceTarget) -> Option<SandboxVenv> {
    let venv_dir = target.source_dir.join(".venv");
    let host_python = venv_interpreter_path(&venv_dir);
    if !host_python.exists() {
        return None;
    }
    Some(SandboxVenv {
        host_python,
        guest_python: GUEST_VENV_PYTHON.to_string(),
        base_install: venv_base_install(&venv_dir)?,
    })
}

/// Host path of a venv's interpreter. `uv venv` lays the interpreter out under
/// `Scripts\python.exe` on Windows and `bin/python` everywhere else (matching
/// `lockfile.rs`).
fn venv_interpreter_path(venv_dir: &Path) -> PathBuf {
    if cfg!(target_os = "windows") {
        venv_dir.join("Scripts").join("python.exe")
    } else {
        venv_dir.join("bin").join("python")
    }
}

/// Resolve the base CPython install root for a venv. Prefers the `home` key in
/// `pyvenv.cfg` (which points at the base interpreter's `bin/`, whose parent is
/// the install root); falls back to canonicalizing the venv `python` symlink.
fn venv_base_install(venv_dir: &Path) -> Option<PathBuf> {
    if let Ok(cfg) = std::fs::read_to_string(venv_dir.join("pyvenv.cfg")) {
        for line in cfg.lines() {
            if let Some((key, value)) = line.split_once('=')
                && key.trim() == "home"
            {
                let home = PathBuf::from(value.trim());
                // `home` is the base interpreter's bin/ — surface its parent
                // (the install root) so stdlib under lib/ is also present.
                return Some(home.parent().map(|p| p.to_path_buf()).unwrap_or(home));
            }
        }
    }
    // Fallback: follow the venv python symlink to the real base binary.
    std::fs::canonicalize(venv_interpreter_path(venv_dir))
        .ok()
        .and_then(|real| real.parent().map(|bin| bin.to_path_buf())) // …/bin
        .and_then(|bin| bin.parent().map(|root| root.to_path_buf())) // install root
}

/// Resolve the install root for a managed toolchain interpreter so the launcher
/// can surface the whole install (`bin/` + `lib/`), not just the binary file.
///
/// nacelle's managed interpreters live at `<root>/bin/<exe>` — e.g.
/// `~/.capsule/toolchains/python-3.11/python/bin/python3` or
/// `~/.ato/toolchains/node-20/<dist>/bin/node`. The runtime loader needs
/// siblings under `<root>/lib/` (libpython*.so, node's `lib/`, …); surfacing only
/// the binary leaves those absent and the interpreter cannot start
/// (`error while loading shared libraries: libpython3.x.so.1.0`).
///
/// Returns `None` (caller keeps the binary-only visibility) when the path is not
/// in a `<root>/bin/<exe>` layout, or when the resolved root would be `/` or
/// `/usr` — those are already provided by the backend's system paths, and
/// surfacing them here would be redundant or over-broad.
fn toolchain_install_root(toolchain_path: &Path) -> Option<PathBuf> {
    let bin = toolchain_path.parent()?;
    if bin.file_name().and_then(|n| n.to_str()) != Some("bin") {
        return None;
    }
    let root = bin.parent()?;
    if root == Path::new("/") || root == Path::new("/usr") {
        return None;
    }
    Some(root.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target_with_source(dir: &Path) -> SourceTarget {
        SourceTarget {
            language: "python".to_string(),
            source_dir: dir.to_path_buf(),
            ..Default::default()
        }
    }

    fn make_venv(root: &Path, home: &Path) {
        let bin = root.join(".venv").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("python"), b"#!/bin/sh\n").unwrap();
        std::fs::write(
            root.join(".venv").join("pyvenv.cfg"),
            format!("home = {}\n", home.display()),
        )
        .unwrap();
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn venv_absent_when_no_venv() {
        let tmp = tempfile::tempdir().unwrap();
        let target = target_with_source(tmp.path());
        // No `.venv` → fall back to the managed base toolchain interpreter.
        assert!(sandbox_venv_python(&target).is_none());
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn venv_uses_guest_path_host_path_and_base_install() {
        let tmp = tempfile::tempdir().unwrap();
        // pyvenv.cfg `home` points at the base interpreter's bin/; the install
        // root is its parent (…/uv-python).
        let base_bin = tmp.path().join("uv-python").join("bin");
        make_venv(tmp.path(), &base_bin);
        let target = target_with_source(tmp.path());

        let venv = sandbox_venv_python(&target).expect("venv must be detected");
        assert_eq!(venv.guest_python, "/app/.venv/bin/python");
        assert_eq!(venv.host_python, tmp.path().join(".venv/bin/python"));
        assert_eq!(venv.base_install, tmp.path().join("uv-python"));
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn base_install_prefers_pyvenv_cfg_home() {
        let tmp = tempfile::tempdir().unwrap();
        let base_bin = PathBuf::from("/opt/cpython-3.11/bin");
        make_venv(tmp.path(), &base_bin);
        let resolved = venv_base_install(&tmp.path().join(".venv")).unwrap();
        assert_eq!(resolved, PathBuf::from("/opt/cpython-3.11"));
    }

    #[test]
    fn toolchain_install_root_resolves_bin_parent() {
        assert_eq!(
            toolchain_install_root(&PathBuf::from(
                "/home/u/.capsule/toolchains/python-3.11/python/bin/python3"
            )),
            Some(PathBuf::from(
                "/home/u/.capsule/toolchains/python-3.11/python"
            ))
        );
        assert_eq!(
            toolchain_install_root(&PathBuf::from(
                "/home/u/.ato/toolchains/node-20/dist/bin/node"
            )),
            Some(PathBuf::from("/home/u/.ato/toolchains/node-20/dist"))
        );
    }

    #[test]
    fn toolchain_install_root_skips_system_and_nonbin_layouts() {
        // System interpreters under /usr or / are already visible.
        assert_eq!(
            toolchain_install_root(&PathBuf::from("/usr/bin/python3")),
            None
        );
        assert_eq!(toolchain_install_root(&PathBuf::from("/bin/python3")), None);
        // Not a `<root>/bin/<exe>` layout.
        assert_eq!(toolchain_install_root(&PathBuf::from("/opt/python3")), None);
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn compute_collects_venv_base_and_toolchain_root() {
        let tmp = tempfile::tempdir().unwrap();
        let base_bin = tmp.path().join("uv-python").join("bin");
        make_venv(tmp.path(), &base_bin);
        let target = target_with_source(tmp.path());

        let toolchain = PathBuf::from("/home/u/.ato/toolchains/python-3.11/python/bin/python3");
        let plan = WorkloadVisibilityPlan::compute(&target, &toolchain);

        let venv = plan.venv.expect("venv must be detected");
        assert_eq!(venv.guest_python, "/app/.venv/bin/python");
        assert_eq!(
            plan.read_paths,
            vec![
                tmp.path().join("uv-python"),
                PathBuf::from("/home/u/.ato/toolchains/python-3.11/python"),
            ]
        );
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn compute_without_venv_still_surfaces_toolchain_root() {
        let tmp = tempfile::tempdir().unwrap();
        let target = target_with_source(tmp.path());
        let toolchain = PathBuf::from("/home/u/.ato/toolchains/python-3.11/python/bin/python3");
        let plan = WorkloadVisibilityPlan::compute(&target, &toolchain);

        assert!(plan.venv.is_none());
        assert_eq!(
            plan.read_paths,
            vec![PathBuf::from("/home/u/.ato/toolchains/python-3.11/python")]
        );
    }

    #[test]
    fn compute_dedupes_overlapping_paths() {
        // A venv whose base install coincides with the toolchain root must not
        // be surfaced twice.
        let tmp = tempfile::tempdir().unwrap();
        let toolchain_root = tmp.path().join("toolchain");
        let base_bin = toolchain_root.join("bin");
        std::fs::create_dir_all(&base_bin).unwrap();
        make_venv(tmp.path(), &base_bin);
        let target = target_with_source(tmp.path());

        let toolchain = base_bin.join("python3");
        let plan = WorkloadVisibilityPlan::compute(&target, &toolchain);
        assert_eq!(plan.read_paths, vec![toolchain_root]);
    }
}
