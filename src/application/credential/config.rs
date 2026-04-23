use std::path::Path;

/// Default backend resolution order: env → memory → age.
pub(crate) fn default_order() -> Vec<String> {
    vec!["env".into(), "memory".into(), "age".into()]
}

/// Read `~/.ato/config.toml` and return `[credentials] order` if present.
///
/// Allowed backend names: `"env"`, `"memory"`, `"age"`.
/// If the key is absent or the file doesn't exist, returns `None` (use default).
///
/// The legacy `[secrets] backends` section (from pre-v0.5.x) is **not** read —
/// users must migrate to `[credentials] order`.
pub(crate) fn read_order(home: &Path) -> Option<Vec<String>> {
    let config_path = home.join(".ato").join("config.toml");
    let raw = std::fs::read_to_string(config_path).ok()?;
    let doc: toml::Value = raw.parse().ok()?;
    let order = doc
        .get("credentials")?
        .get("order")?
        .as_array()?
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_ascii_lowercase()))
        .collect::<Vec<_>>();
    if order.is_empty() {
        None
    } else {
        Some(order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn returns_none_when_config_missing() {
        let dir = TempDir::new().unwrap();
        assert!(read_order(dir.path()).is_none());
    }

    #[test]
    fn reads_credentials_order() {
        let dir = TempDir::new().unwrap();
        let cfg_dir = dir.path().join(".ato");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            r#"[credentials]
order = ["memory", "age"]
"#,
        )
        .unwrap();
        assert_eq!(
            read_order(dir.path()),
            Some(vec!["memory".into(), "age".into()])
        );
    }

    #[test]
    fn legacy_secrets_backends_ignored() {
        let dir = TempDir::new().unwrap();
        let cfg_dir = dir.path().join(".ato");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            r#"[secrets]
backends = ["memory"]
"#,
        )
        .unwrap();
        assert_eq!(read_order(dir.path()), None);
    }
}
