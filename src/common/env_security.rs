//! Env key safety validation for user-supplied environment variables.
//!
//! Prevents dangerous env keys from being injected into capsule processes via
//! `--env-file`, interactive prompts, or cached env stores.

use anyhow::Result;

/// Env keys that directly control OS-level loader behaviour and can be used to
/// inject arbitrary code into any child process.  These are unconditionally
/// rejected when loading user-supplied env values.
const HARD_DENIED_ENV_KEYS: &[&str] = &[
    // Linux dynamic linker hijack
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
    "LD_DEBUG",
    // macOS dynamic linker hijack
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    "DYLD_FORCE_FLAT_NAMESPACE",
    "DYLD_IMAGE_SUFFIX",
    // Python startup code injection
    "PYTHONSTARTUP",
    // npm config overrides — can redirect script execution or change the registry
    "npm_config_globalconfig",
    "npm_config_userconfig",
    "npm_config_global_prefix",
    "npm_config_registry",
    "npm_config_script_shell",
];

/// `NODE_OPTIONS` flags that enable module-load-time code injection.  A
/// `NODE_OPTIONS` value containing any of these sub-strings is rejected.
const NODE_OPTIONS_INJECTION_FLAGS: &[&str] = &[
    "--require",
    "-r ",
    "-r\t",
    "--loader",
    "--experimental-loader",
    "--import",
    "--experimental-vm-modules",
    "--experimental-specifier-resolution",
];

/// Validate that a single user-supplied `(key, value)` pair is safe to inject
/// into a capsule process.
///
/// Returns `Err` with a descriptive message if the key or value is rejected.
/// Returns `Ok(())` if the pair is safe to use.
pub fn check_user_env_safety(key: &str, value: &str) -> Result<()> {
    if HARD_DENIED_ENV_KEYS.contains(&key) {
        anyhow::bail!(
            "env key '{}' is blocked for security: this key controls OS-level process loading \
             and cannot be set via --env-file or interactive prompts.\n\
             If you need this for a legitimate purpose, set it in your shell before running `ato`.",
            key
        );
    }

    // NODE_OPTIONS is commonly used for memory tuning (--max-old-space-size) but
    // also enables module injection via --require / --loader.  Reject values that
    // contain injection-capable flags while allowing benign memory/optimisation flags.
    if key == "NODE_OPTIONS" {
        let lower = value.to_ascii_lowercase();
        for flag in NODE_OPTIONS_INJECTION_FLAGS {
            if lower.contains(flag) {
                anyhow::bail!(
                    "NODE_OPTIONS value contains a potentially dangerous flag ('{}').\n\
                     Module-loading flags in NODE_OPTIONS are blocked for security.\n\
                     Allowed examples: --max-old-space-size=4096, --expose-gc.",
                    flag.trim()
                );
            }
        }
    }

    Ok(())
}

/// Validate all pairs in a slice.  Fails fast on the first rejected key.
#[allow(dead_code)]
pub fn check_user_env_pairs(pairs: &[(String, String)]) -> Result<()> {
    for (key, value) in pairs {
        check_user_env_safety(key, value)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hard_denied_keys_are_rejected() {
        for key in HARD_DENIED_ENV_KEYS {
            let result = check_user_env_safety(key, "anything");
            assert!(result.is_err(), "expected '{}' to be denied", key);
            assert!(result.unwrap_err().to_string().contains("blocked for security"));
        }
    }

    #[test]
    fn node_options_injection_flags_are_rejected() {
        let dangerous = [
            "--require /evil.js",
            "--loader ts-node/esm --require hook",
            "--experimental-loader malicious",
            "--import /path/to/inject.js",
        ];
        for value in &dangerous {
            let result = check_user_env_safety("NODE_OPTIONS", value);
            assert!(
                result.is_err(),
                "expected NODE_OPTIONS='{}' to be denied",
                value
            );
        }
    }

    #[test]
    fn node_options_benign_values_are_allowed() {
        let safe = [
            "--max-old-space-size=4096",
            "--max-semi-space-size=128",
            "--expose-gc",
            "--no-experimental-fetch",
        ];
        for value in &safe {
            check_user_env_safety("NODE_OPTIONS", value)
                .unwrap_or_else(|e| panic!("expected NODE_OPTIONS='{}' to be allowed: {}", value, e));
        }
    }

    #[test]
    fn normal_env_keys_are_allowed() {
        let pairs = [
            ("OPENAI_API_KEY", "sk-test123"),
            ("DATABASE_URL", "postgres://localhost/db"),
            ("PORT", "3000"),
            ("API_BASE_URL", "https://api.example.com"),
        ];
        for (key, value) in &pairs {
            check_user_env_safety(key, value)
                .unwrap_or_else(|e| panic!("expected '{}' to be allowed: {}", key, e));
        }
    }

    #[test]
    fn check_pairs_fails_fast_on_first_bad_key() {
        let pairs = vec![
            ("OPENAI_API_KEY".to_string(), "sk-ok".to_string()),
            ("LD_PRELOAD".to_string(), "/evil.so".to_string()),
            ("DATABASE_URL".to_string(), "postgres://localhost".to_string()),
        ];
        let result = check_user_env_pairs(&pairs);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("LD_PRELOAD"));
    }
}
