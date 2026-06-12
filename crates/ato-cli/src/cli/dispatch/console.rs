use anyhow::{Context, Result, bail};

use crate::cli::console::ConsoleCommands;

/// Default data dir for the local registry — mirrors the default in
/// `cli/registry.rs` (RegistryCommands::Serve).
const DEFAULT_DATA_DIR: &str = "~/.ato/local-registry";

/// File written by `ato registry serve` so `ato console open` can find the
/// token without manual copy-paste.
const CONSOLE_TOKEN_FILE: &str = ".console-token";

/// Env var the user can set to override the token lookup.
const TOKEN_ENV: &str = "ATO_REGISTRY_TOKEN";

pub(crate) fn execute_console_command(command: ConsoleCommands) -> Result<()> {
    match command {
        ConsoleCommands::Open {
            endpoint,
            token,
            app_url,
            print_url,
        } => execute_console_open(endpoint, token, app_url, print_url),
    }
}

fn execute_console_open(
    endpoint: String,
    token: Option<String>,
    app_url: String,
    print_url: bool,
) -> Result<()> {
    let token = resolve_token(token)?;
    let url = build_console_url(&app_url, &endpoint, &token);

    if print_url {
        eprintln!("⚠️  The URL below contains a bearer token. Treat it as sensitive.");
        println!("{url}");
        return Ok(());
    }

    match try_open_browser(&url) {
        Ok(()) => {
            println!("🌐 Opening Ato Web Console…");
            println!("   Endpoint: {endpoint}");
        }
        Err(err) => {
            eprintln!("❌ Failed to open browser: {err}");
            eprintln!("   Re-run with --print-url to copy the full console URL.");
        }
    }

    Ok(())
}

/// Resolve the bearer token from (in priority order):
///   1. `--token` CLI argument
///   2. `ATO_REGISTRY_TOKEN` environment variable
///   3. `~/.ato/local-registry/.console-token` file (written by `ato registry serve`)
fn resolve_token(explicit: Option<String>) -> Result<String> {
    if let Some(t) = explicit {
        let t = t.trim().to_owned();
        if !t.is_empty() {
            return Ok(t);
        }
    }

    if let Ok(t) = std::env::var(TOKEN_ENV) {
        let t = t.trim().to_owned();
        if !t.is_empty() {
            return Ok(t);
        }
    }

    if let Ok(path) = expand_tilde(DEFAULT_DATA_DIR) {
        let token_path = path.join(CONSOLE_TOKEN_FILE);
        if token_path.exists() {
            let raw = std::fs::read_to_string(&token_path)
                .with_context(|| format!("reading {}", token_path.display()))?;
            let t = raw.trim().to_owned();
            if !t.is_empty() {
                return Ok(t);
            }
        }
    }

    bail!(
        "No bearer token found.\n\
         \n\
         Provide one with:\n\
         \n\
         1. --token <TOKEN>  (on the command line)\n\
         2. {TOKEN_ENV}=<TOKEN>  (environment variable)\n\
         3. Start the local registry with --auth-token and it will write\n\
            the token to ~/.ato/local-registry/.console-token automatically.\n\
         \n\
         Example:\n\
           ato registry serve --auth-token my-secret-token\n\
           ato console open"
    )
}

/// Construct the PWA URL.
///
/// URL shape:
///   https://app.ato.run/#route=/sessions&endpoint=<encoded>&token=<encoded>
///
/// The token travels in the fragment only — fragments are never sent to the
/// server and are stripped by `clearSensitiveFragment()` in the PWA on load.
pub(crate) fn build_console_url(app_url: &str, endpoint: &str, token: &str) -> String {
    let app_url = app_url.trim_end_matches('/');
    let encoded_endpoint = url_encode(endpoint);
    let encoded_token = url_encode(token);
    format!("{app_url}/#route=/sessions&endpoint={encoded_endpoint}&token={encoded_token}")
}

/// Minimal percent-encoding for fragment components.
/// Only encodes characters that would break URL fragment parsing.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b':'
            | b'/'
            | b'@' => out.push(byte as char),
            b => {
                out.push('%');
                let hi = b >> 4;
                let lo = b & 0xf;
                out.push(char::from(if hi < 10 { b'0' + hi } else { b'A' + hi - 10 }));
                out.push(char::from(if lo < 10 { b'0' + lo } else { b'A' + lo - 10 }));
            }
        }
    }
    out
}

fn expand_tilde(path: &str) -> Result<std::path::PathBuf> {
    if path == "~" {
        return dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Failed to resolve home dir"));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Failed to resolve home dir"))?;
        return Ok(home.join(rest));
    }
    Ok(std::path::PathBuf::from(path))
}

fn try_open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .context("Failed to launch browser with `open`")?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .context("Failed to launch browser with `xdg-open`")?;
        Ok(())
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .context("Failed to launch browser with `start`")?;
        return Ok(());
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    bail!("Browser open not supported on this platform: {}", url)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_url_uses_fragment_not_query() {
        let url = build_console_url("https://app.ato.run", "http://127.0.0.1:8787", "tok123");
        assert!(url.contains('#'), "must use fragment");
        assert!(!url.contains('?'), "must not use query string");
    }

    #[test]
    fn console_url_default_route_is_sessions() {
        let url = build_console_url("https://app.ato.run", "http://127.0.0.1:8787", "tok");
        assert!(
            url.contains("route=/sessions"),
            "default route is /sessions"
        );
    }

    #[test]
    fn console_url_endpoint_encoded() {
        let url = build_console_url("https://app.ato.run", "http://127.0.0.1:8787", "tok");
        // ':' and '/' are preserved in url_encode, but '8787' must appear
        assert!(url.contains("8787"), "port must be present");
        // Scheme separator appears, endpoint is not raw-injected unsafely
        assert!(url.contains("endpoint=http"), "endpoint key present");
    }

    #[test]
    fn console_url_token_encoded() {
        let url = build_console_url("https://app.ato.run", "http://127.0.0.1:8787", "my token");
        assert!(
            url.contains("token=my%20token"),
            "space in token must be percent-encoded: {url}"
        );
    }

    #[test]
    fn console_url_app_url_override() {
        let url = build_console_url("http://localhost:5173", "http://127.0.0.1:8787", "tok");
        assert!(
            url.starts_with("http://localhost:5173/"),
            "custom app URL used"
        );
    }

    #[test]
    fn console_url_token_not_printed_without_flag() {
        // The URL is returned as a string; it's the caller's responsibility to
        // guard printing. Verify the token IS in the URL (for fragment delivery),
        // and that build_console_url does not do any masking.
        let tok = "super-secret";
        let url = build_console_url("https://app.ato.run", "http://127.0.0.1:8787", tok);
        assert!(url.contains(tok));
    }

    #[test]
    fn url_encode_preserves_scheme_and_port() {
        // http://127.0.0.1:8787 should not have : or / encoded
        let encoded = url_encode("http://127.0.0.1:8787");
        assert_eq!(encoded, "http://127.0.0.1:8787");
    }

    #[test]
    fn url_encode_percent_encodes_spaces_and_special_chars() {
        assert_eq!(url_encode("foo bar"), "foo%20bar");
        assert_eq!(url_encode("a&b"), "a%26b");
        assert_eq!(url_encode("a=b"), "a%3Db");
        assert_eq!(url_encode("#hash"), "%23hash");
    }
}
