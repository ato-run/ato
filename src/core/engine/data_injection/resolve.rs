use std::collections::BTreeMap;

use anyhow::Result;

pub(super) fn parse_cli_bindings(raw_bindings: &[String]) -> Result<BTreeMap<String, String>> {
    let mut bindings = BTreeMap::new();
    for raw_binding in raw_bindings {
        let (key, value) = raw_binding.split_once('=').ok_or_else(|| {
            anyhow::anyhow!("--inject must use KEY=VALUE syntax, got '{}'", raw_binding)
        })?;
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            anyhow::bail!(
                "--inject must use non-empty KEY=VALUE syntax, got '{}'",
                raw_binding
            );
        }
        if bindings
            .insert(key.to_string(), value.to_string())
            .is_some()
        {
            anyhow::bail!("duplicate --inject key '{}'", key);
        }
    }
    Ok(bindings)
}
