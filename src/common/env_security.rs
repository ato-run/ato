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
    // Python module path redirect — can shadow stdlib or inject packages
    "PYTHONPATH",
    // Shell startup file executed by non-interactive bash (used in sh -lc build cmds)
    "BASH_ENV",
    // Node.js module resolution redirect — can hijack require() calls
    "NODE_PATH",
    // npm config overrides — can redirect script execution or change the registry
    "npm_config_globalconfig",
    "npm_config_userconfig",
    "npm_config_global_prefix",
    "npm_config_registry",
    "npm_config_script_shell",
];

/// `NODE_OPTIONS` flags that enable module-load-time code injection.  A
/// `NODE_OPTIONS` value containing any of these sub-strings is rejected.
///
/// All entries MUST be lowercase — `check_user_env_safety` lowercases the value
/// before comparing.
const NODE_OPTIONS_INJECTION_FLAGS: &[&str] = &[
    "--require",
    "-r ",  // space-separated: NODE_OPTIONS="-r ./hook.js"
    "-r\t", // tab-separated
    "-r=",  // equals-separated: NODE_OPTIONS="-r=./malicious.js"
    "--loader",
    "--experimental-loader",
    "--import ",  // whitespace-separated: NODE_OPTIONS="--import ./inject.js"
    "--import\t", // tab-separated
    "--import=",  // equals-separated: NODE_OPTIONS="--import=./inject.js"
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
            // All flags in the list must be lowercase — assert during development.
            debug_assert!(
                flag.chars()
                    .all(|c| c.is_ascii_lowercase() || !c.is_ascii_alphabetic()),
                "NODE_OPTIONS_INJECTION_FLAGS entry '{}' contains uppercase letters",
                flag
            );
            if lower.contains(flag) {
                anyhow::bail!(
                    "NODE_OPTIONS value contains a potentially dangerous flag ('{}').\n\
                     Module-loading flags in NODE_OPTIONS are blocked for security.\n\
                     Allowed examples: --max-old-space-size=4096, --expose-gc.",
                    flag.trim()
                );
            }
        }
        // Also catch bare --require or --import at end-of-string (no trailing separator).
        for bare in &["--require", "-r", "--import"] {
            if lower == *bare
                || lower.ends_with(&format!(" {}", bare))
                || lower.ends_with(&format!("\t{}", bare))
            {
                anyhow::bail!(
                    "NODE_OPTIONS value contains a potentially dangerous flag ('{}').\n\
                     Module-loading flags in NODE_OPTIONS are blocked for security.\n\
                     Allowed examples: --max-old-space-size=4096, --expose-gc.",
                    bare
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
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("blocked for security"));
        }
    }

    #[test]
    fn node_options_injection_flags_are_rejected() {
        let dangerous = [
            "--require /evil.js",
            "--loader ts-node/esm --require hook",
            "--experimental-loader malicious",
            // P1: --import variations
            "--import /path/to/inject.js",
            "--import=./inject.mjs",
            "--import\t./inject.mjs",
            // P0: -r= bypass
            "-r=./malicious.js",
            "-r /evil.js",
            "-r\t/evil.js",
            // bare flags at end-of-string
            "--require",
            "-r",
            "--import",
            "--max-old-space-size=4096 --require",
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
            check_user_env_safety("NODE_OPTIONS", value).unwrap_or_else(|e| {
                panic!("expected NODE_OPTIONS='{}' to be allowed: {}", value, e)
            });
        }
    }

    #[test]
    fn new_denied_keys_are_rejected() {
        for key in &["BASH_ENV", "NODE_PATH", "PYTHONPATH"] {
            let result = check_user_env_safety(key, "anything");
            assert!(result.is_err(), "expected '{}' to be denied", key);
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
            (
                "DATABASE_URL".to_string(),
                "postgres://localhost".to_string(),
            ),
        ];
        let result = check_user_env_pairs(&pairs);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("LD_PRELOAD"));
    }
}
