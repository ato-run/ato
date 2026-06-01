use std::fmt;
use std::path::{Path, PathBuf};

pub const ATO_BIN_ENV: &str = "ATO_DESKTOP_ATO_BIN";
pub const ASSETS_DIR_ENV: &str = "ATO_DESKTOP_ASSETS_DIR";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopPlatform {
    Macos,
    Windows,
    Unix,
}

impl DesktopPlatform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }

    fn executable_name(self, name: &str) -> String {
        if self == Self::Windows && !name.to_ascii_lowercase().ends_with(".exe") {
            format!("{name}.exe")
        } else {
            name.to_string()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleResolutionError {
    subject: &'static str,
    searched_paths: Vec<PathBuf>,
    hint: String,
}

impl BundleResolutionError {
    fn new(subject: &'static str, searched_paths: Vec<PathBuf>, hint: impl Into<String>) -> Self {
        Self {
            subject,
            searched_paths,
            hint: hint.into(),
        }
    }

    pub fn searched_paths(&self) -> &[PathBuf] {
        &self.searched_paths
    }
}

impl fmt::Display for BundleResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} was not found.", self.subject)?;
        if !self.searched_paths.is_empty() {
            writeln!(f, "Searched paths:")?;
            for path in &self.searched_paths {
                writeln!(f, "  - {}", path.display())?;
            }
        }
        write!(f, "{}", self.hint)
    }
}

impl std::error::Error for BundleResolutionError {}

#[derive(Clone, Debug)]
pub struct DesktopBundlePaths {
    platform: DesktopPlatform,
    current_exe: Option<PathBuf>,
    current_dir: Option<PathBuf>,
    ato_bin_env: Option<PathBuf>,
    assets_dir_env: Option<PathBuf>,
    path_entries: Vec<PathBuf>,
    manifest_dir: Option<PathBuf>,
}

impl DesktopBundlePaths {
    pub fn from_env() -> Self {
        let path_entries = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default();
        Self {
            platform: DesktopPlatform::current(),
            current_exe: std::env::current_exe().ok(),
            current_dir: std::env::current_dir().ok(),
            ato_bin_env: std::env::var_os(ATO_BIN_ENV).map(PathBuf::from),
            assets_dir_env: std::env::var_os(ASSETS_DIR_ENV).map(PathBuf::from),
            path_entries,
            manifest_dir: option_env!("CARGO_MANIFEST_DIR").map(PathBuf::from),
        }
    }

    #[cfg(test)]
    pub fn for_test(
        platform: DesktopPlatform,
        current_exe: impl Into<PathBuf>,
        current_dir: Option<PathBuf>,
        ato_bin_env: Option<PathBuf>,
        assets_dir_env: Option<PathBuf>,
        path_entries: Vec<PathBuf>,
        manifest_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            platform,
            current_exe: Some(current_exe.into()),
            current_dir,
            ato_bin_env,
            assets_dir_env,
            path_entries,
            manifest_dir,
        }
    }

    pub fn resolve_ato_helper_with_extra<I>(
        &self,
        extra_candidates: I,
    ) -> Result<PathBuf, BundleResolutionError>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut searched = Vec::new();

        if let Some(path) = &self.ato_bin_env {
            searched.push(path.clone());
            if path.is_file() {
                return Ok(path.clone());
            }
            return Err(BundleResolutionError::new(
                "ato helper binary",
                searched,
                format!("{ATO_BIN_ENV} points to a missing file. Update the override or unset it."),
            ));
        }

        for candidate in self.bundle_binary_candidates("ato") {
            searched.push(candidate.clone());
            if candidate.is_file() {
                return Ok(candidate);
            }
        }

        for candidate in extra_candidates {
            searched.push(candidate.clone());
            if candidate.is_file() {
                return Ok(candidate);
            }
        }

        for candidate in self.path_candidates("ato") {
            searched.push(candidate.clone());
            if candidate.is_file() {
                return Ok(candidate);
            }
        }

        Err(BundleResolutionError::new(
            "ato helper binary",
            searched,
            format!(
                "Bundle the helper next to ato-desktop, set {ATO_BIN_ENV}, or install 'ato' on PATH."
            ),
        ))
    }

    pub fn locate_bundled_ato_helper(&self) -> Result<PathBuf, BundleResolutionError> {
        let mut searched = Vec::new();
        for candidate in self.bundle_binary_candidates("ato") {
            searched.push(candidate.clone());
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        Err(BundleResolutionError::new(
            "bundled ato helper binary",
            searched,
            "This packaged build is missing its bundled CLI helper.",
        ))
    }

    pub fn resolve_assets_dir(&self) -> Result<PathBuf, BundleResolutionError> {
        let mut searched = Vec::new();

        if let Some(path) = &self.assets_dir_env {
            searched.push(path.clone());
            if path.is_dir() {
                return Ok(path.clone());
            }
            return Err(BundleResolutionError::new(
                "ato-desktop assets directory",
                searched,
                format!(
                    "{ASSETS_DIR_ENV} points to a missing directory. Update the override or unset it."
                ),
            ));
        }

        for candidate in self.assets_candidates() {
            searched.push(candidate.clone());
            if candidate.is_dir() {
                return Ok(candidate);
            }
        }

        Err(BundleResolutionError::new(
            "ato-desktop assets directory",
            searched,
            format!(
                "Bundle assets with ato-desktop or set {ASSETS_DIR_ENV} to the assets directory."
            ),
        ))
    }

    pub fn path_candidates(&self, binary: &str) -> Vec<PathBuf> {
        let name = self.platform.executable_name(binary);
        self.path_entries
            .iter()
            .map(|entry| entry.join(&name))
            .collect()
    }

    pub fn bundle_binary_candidates(&self, binary: &str) -> Vec<PathBuf> {
        let Some(exe_dir) = self.current_exe.as_deref().and_then(Path::parent) else {
            return Vec::new();
        };
        let binary_name = self.platform.executable_name(binary);

        let mut candidates = vec![
            exe_dir.join("Helpers").join(&binary_name),
            exe_dir.join(&binary_name),
            exe_dir.join("bin").join(&binary_name),
        ];

        if self.platform == DesktopPlatform::Macos
            && let Some(contents) = exe_dir.parent()
        {
            candidates.insert(0, contents.join("Helpers").join(&binary_name));
        }

        candidates
    }

    fn assets_candidates(&self) -> Vec<PathBuf> {
        let mut candidates = Vec::new();

        if let Some(exe_dir) = self.current_exe.as_deref().and_then(Path::parent) {
            candidates.push(exe_dir.join("assets"));
            candidates.push(exe_dir.join("resources").join("assets"));
            candidates.push(exe_dir.join("Resources"));
            candidates.push(exe_dir.join("Resources").join("assets"));
            if let Some(contents) = exe_dir.parent() {
                candidates.push(contents.join("Resources").join("assets"));
            }
        }

        if let Some(cwd) = &self.current_dir {
            candidates.push(cwd.join("assets"));
        }
        if let Some(manifest_dir) = &self.manifest_dir {
            candidates.push(manifest_dir.join("assets"));
        }

        candidates
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn windows_install_tree_resolves_bin_helper_and_assets() {
        let root = test_root("windows-install-tree");
        let install = root.join("Ato");
        let bin = install.join("bin");
        let assets = install.join("assets");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&assets).unwrap();
        fs::write(install.join("ato-desktop.exe"), "").unwrap();
        fs::write(bin.join("ato.exe"), "").unwrap();

        let resolver = DesktopBundlePaths::for_test(
            DesktopPlatform::Windows,
            install.join("ato-desktop.exe"),
            None,
            None,
            None,
            Vec::new(),
            None,
        );

        assert_eq!(
            resolver.resolve_ato_helper_with_extra([]).unwrap(),
            bin.join("ato.exe")
        );
        assert_eq!(resolver.resolve_assets_dir().unwrap(), assets);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn windows_path_lookup_checks_exe_suffix() {
        let root = test_root("windows-path-lookup");
        let path_dir = root.join("path");
        fs::create_dir_all(&path_dir).unwrap();
        fs::write(path_dir.join("ato.exe"), "").unwrap();

        let resolver = DesktopBundlePaths::for_test(
            DesktopPlatform::Windows,
            root.join("Ato").join("ato-desktop.exe"),
            None,
            None,
            None,
            vec![path_dir.clone()],
            None,
        );

        assert_eq!(
            resolver.resolve_ato_helper_with_extra([]).unwrap(),
            path_dir.join("ato.exe")
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn explicit_env_overrides_win_before_bundled_layout() {
        let root = test_root("env-overrides");
        let install = root.join("Ato");
        let override_dir = root.join("override");
        fs::create_dir_all(install.join("bin")).unwrap();
        fs::create_dir_all(&override_dir).unwrap();
        fs::write(install.join("ato-desktop.exe"), "").unwrap();
        fs::write(install.join("bin").join("ato.exe"), "").unwrap();
        fs::write(override_dir.join("ato.exe"), "").unwrap();

        let resolver = DesktopBundlePaths::for_test(
            DesktopPlatform::Windows,
            install.join("ato-desktop.exe"),
            None,
            Some(override_dir.join("ato.exe")),
            Some(override_dir.clone()),
            Vec::new(),
            None,
        );

        assert_eq!(
            resolver.resolve_ato_helper_with_extra([]).unwrap(),
            override_dir.join("ato.exe")
        );
        assert_eq!(resolver.resolve_assets_dir().unwrap(), override_dir);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn macos_bundle_prefers_contents_helpers() {
        let root = test_root("macos-bundle");
        let app = root.join("Ato Desktop.app");
        let macos_dir = app.join("Contents").join("MacOS");
        let helpers_dir = app.join("Contents").join("Helpers");
        fs::create_dir_all(&macos_dir).unwrap();
        fs::create_dir_all(&helpers_dir).unwrap();
        fs::write(macos_dir.join("ato-desktop"), "").unwrap();
        fs::write(helpers_dir.join("ato"), "").unwrap();

        let resolver = DesktopBundlePaths::for_test(
            DesktopPlatform::Macos,
            macos_dir.join("ato-desktop"),
            None,
            None,
            None,
            Vec::new(),
            None,
        );

        assert_eq!(
            resolver.locate_bundled_ato_helper().unwrap(),
            helpers_dir.join("ato")
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn missing_helper_error_lists_searched_paths() {
        let root = test_root("missing-helper");
        let install = root.join("Ato");
        fs::create_dir_all(&install).unwrap();

        let resolver = DesktopBundlePaths::for_test(
            DesktopPlatform::Windows,
            install.join("ato-desktop.exe"),
            None,
            None,
            None,
            Vec::new(),
            None,
        );

        let error = resolver.resolve_ato_helper_with_extra([]).unwrap_err();
        assert!(
            error
                .searched_paths()
                .iter()
                .any(|path| path.ends_with("ato.exe"))
        );
        assert!(error.to_string().contains("Searched paths:"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn missing_assets_error_lists_searched_paths() {
        let root = test_root("missing-assets");
        let install = root.join("Ato");
        fs::create_dir_all(&install).unwrap();

        let resolver = DesktopBundlePaths::for_test(
            DesktopPlatform::Windows,
            install.join("ato-desktop.exe"),
            None,
            None,
            None,
            Vec::new(),
            None,
        );

        let error = resolver.resolve_assets_dir().unwrap_err();
        assert!(
            error
                .searched_paths()
                .iter()
                .any(|path| path.ends_with("assets"))
        );
        assert!(error.to_string().contains("Searched paths:"));

        fs::remove_dir_all(root).ok();
    }

    fn test_root(name: &str) -> PathBuf {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".tmp")
            .join(format!(
                "{name}-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
        if root.exists() {
            fs::remove_dir_all(&root).ok();
        }
        fs::create_dir_all(&root).unwrap();
        root
    }
}
