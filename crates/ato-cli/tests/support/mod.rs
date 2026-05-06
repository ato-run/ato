use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

pub struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    pub fn set<V: AsRef<OsStr>>(key: &'static str, value: V) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }

    pub fn set_path(key: &'static str, value: &Path) -> Self {
        Self::set(key, value.as_os_str())
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

pub struct IsolatedAto {
    root: TempDir,
    _guards: Vec<EnvVarGuard>,
}

impl IsolatedAto {
    pub fn new() -> Self {
        let root = TempDir::new().expect("create isolated ato tempdir");
        let ato_home = root.path().join("ato-home");
        let home = root.path().join("home");
        let xdg_config_home = root.path().join("xdg-config");
        let xdg_cache_home = root.path().join("xdg-cache");

        std::fs::create_dir_all(&ato_home).expect("create isolated ATO_HOME");
        std::fs::create_dir_all(&home).expect("create isolated HOME");
        std::fs::create_dir_all(&xdg_config_home).expect("create isolated XDG_CONFIG_HOME");
        std::fs::create_dir_all(&xdg_cache_home).expect("create isolated XDG_CACHE_HOME");

        let guards = vec![
            EnvVarGuard::set_path("ATO_HOME", &ato_home),
            EnvVarGuard::set_path("HOME", &home),
            EnvVarGuard::set_path("XDG_CONFIG_HOME", &xdg_config_home),
            EnvVarGuard::set_path("XDG_CACHE_HOME", &xdg_cache_home),
        ];

        Self {
            root,
            _guards: guards,
        }
    }

    pub fn path(&self) -> &Path {
        self.root.path()
    }

    #[allow(dead_code)]
    pub fn ato_home(&self) -> PathBuf {
        self.root.path().join("ato-home")
    }
}
