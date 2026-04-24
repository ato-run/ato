use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rand::Rng;

/// Response from a curl-based HTTP call.
pub(crate) struct CurlUploadResponse {
    pub(crate) status: u16,
    pub(crate) body: String,
    pub(crate) headers: HashMap<String, String>,
}

impl CurlUploadResponse {
    pub(crate) fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Exponential-backoff retry policy for transient failures
/// (curl transport errors, 5xx responses, Cloudflare `error code: 1102`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct RetryPolicy {
    pub(crate) max_retries: u32,
    pub(crate) base_delay_ms: u64,
    pub(crate) multiplier: f64,
    pub(crate) jitter: f64,
}

impl RetryPolicy {
    /// Default: 3 retries at roughly 200ms / 600ms / 1800ms (±25% jitter).
    pub(crate) const fn default_policy() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 200,
            multiplier: 3.0,
            jitter: 0.25,
        }
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::default_policy()
    }
}

fn retry_disabled() -> bool {
    std::env::var("ATO_DISABLE_RETRY")
        .map(|v| {
            let t = v.trim();
            !t.is_empty() && !t.eq_ignore_ascii_case("0") && !t.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

/// Returns true when a response indicates a transient server-side failure
/// that is safe to retry (5xx or Cloudflare Worker `error code: 1102`).
pub(crate) fn is_retryable_response(resp: &CurlUploadResponse) -> bool {
    if (500..600).contains(&resp.status) {
        return true;
    }
    // Cloudflare's 1102 "worker exceeded CPU" sometimes surfaces as 4xx with an
    // HTML error page. Retry when we clearly see the marker and the request
    // returned a failure status — never treat a 2xx body containing the string
    // as retryable.
    if resp.status >= 400 && resp.body.contains("error code: 1102") {
        return true;
    }
    false
}

fn jittered_delay(base_ms: u64, jitter: f64) -> Duration {
    if jitter <= 0.0 || base_ms == 0 {
        return Duration::from_millis(base_ms);
    }
    let factor = {
        let mut rng = rand::thread_rng();
        let r: f64 = rng.gen_range(-jitter..=jitter);
        (1.0 + r).max(0.1)
    };
    let ms = ((base_ms as f64) * factor).max(1.0) as u64;
    Duration::from_millis(ms)
}

fn sanitize_url_for_log(url: &str) -> String {
    match url.split_once('?') {
        Some((path, _)) => path.to_string(),
        None => url.to_string(),
    }
}

/// Run `attempt` with exponential backoff, retrying on transient failures.
///
/// - Returns on the first 2xx response.
/// - Returns the terminal outcome on any non-retryable failure (4xx without
///   the `1102` marker) or after `policy.max_retries + 1` total attempts.
/// - Honors `ATO_DISABLE_RETRY=1` to force a single attempt (primarily useful
///   in tests that want to observe raw failures).
pub(crate) fn with_retry<F>(
    policy: RetryPolicy,
    context: &str,
    mut attempt: F,
) -> Result<CurlUploadResponse>
where
    F: FnMut() -> Result<CurlUploadResponse>,
{
    let max = if retry_disabled() {
        0
    } else {
        policy.max_retries
    };
    let mut next_delay_ms = policy.base_delay_ms;

    for attempt_idx in 0..=max {
        let outcome = attempt();
        match outcome {
            Ok(resp) => {
                if resp.is_success() || !is_retryable_response(&resp) {
                    return Ok(resp);
                }
                if attempt_idx == max {
                    return Ok(resp);
                }
                let slept = jittered_delay(next_delay_ms, policy.jitter);
                eprintln!(
                    "ato: transient HTTP {} from {} (attempt {}/{}); retrying in {}ms",
                    resp.status,
                    context,
                    attempt_idx + 1,
                    max + 1,
                    slept.as_millis(),
                );
                sleep(slept);
                next_delay_ms = ((next_delay_ms as f64) * policy.multiplier) as u64;
            }
            Err(err) => {
                if attempt_idx == max {
                    return Err(err);
                }
                let slept = jittered_delay(next_delay_ms, policy.jitter);
                eprintln!(
                    "ato: curl transport error on {} (attempt {}/{}): {}; retrying in {}ms",
                    context,
                    attempt_idx + 1,
                    max + 1,
                    err,
                    slept.as_millis(),
                );
                sleep(slept);
                next_delay_ms = ((next_delay_ms as f64) * policy.multiplier) as u64;
            }
        }
    }
    unreachable!("with_retry loop exited without return")
}

/// Upload `body` as an HTTP PUT to `url` by shelling out to the `curl` binary.
/// Retries on transient server-side failures (5xx / Cloudflare 1102).
///
/// Using curl sidesteps reqwest's blocking-client timeout handling for large
/// artifact uploads. The payload is written to a tempfile before handing off
/// to curl so we do not need to stream 70+ MB through a pipe.
pub(crate) fn put_bytes(
    url: &str,
    body: &[u8],
    extra_headers: &[(String, String)],
) -> Result<CurlUploadResponse> {
    let log_target = format!("PUT {}", sanitize_url_for_log(url));
    with_retry(RetryPolicy::default_policy(), &log_target, || {
        put_bytes_once(url, body, extra_headers)
    })
}

fn put_bytes_once(
    url: &str,
    body: &[u8],
    extra_headers: &[(String, String)],
) -> Result<CurlUploadResponse> {
    let curl = std::env::var("ATO_CURL_BIN").unwrap_or_else(|_| "curl".to_string());

    let mut body_file =
        tempfile::NamedTempFile::new().context("Failed to create tempfile for curl upload body")?;
    body_file
        .write_all(body)
        .context("Failed to write upload body to tempfile")?;
    body_file
        .flush()
        .context("Failed to flush upload body tempfile")?;
    let body_path = body_file.path().to_path_buf();

    let mut cmd = Command::new(&curl);
    cmd.arg("--silent")
        .arg("--show-error")
        .arg("--max-time")
        .arg("900")
        .arg("--connect-timeout")
        .arg("30")
        .arg("--request")
        .arg("PUT")
        .arg("--header")
        .arg("expect:")
        .arg("--header")
        .arg("content-type: application/octet-stream")
        .arg("--dump-header")
        .arg("-")
        .arg("--write-out")
        .arg("\n__ATO_CURL_HTTP_STATUS__:%{http_code}\n")
        .arg("--data-binary")
        .arg(format!("@{}", body_path.display()));

    for (name, value) in extra_headers {
        cmd.arg("--header").arg(format!("{}: {}", name, value));
    }

    cmd.arg(url).stdout(Stdio::piped()).stderr(Stdio::piped());

    let output = cmd
        .output()
        .with_context(|| format!("Failed to launch curl ({})", curl))?;

    // Keep the tempfile alive until curl exits; drop it here.
    drop(body_file);

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        bail!(
            "curl upload failed (exit={}) stderr={} stdout_tail={}",
            output.status.code().unwrap_or(-1),
            stderr.trim(),
            tail_lines(&stdout, 3)
        );
    }

    let marker = "__ATO_CURL_HTTP_STATUS__:";
    let (before_marker, status_tail) = stdout
        .rsplit_once(marker)
        .ok_or_else(|| anyhow::anyhow!("curl output did not contain HTTP status marker"))?;
    let status = status_tail
        .trim()
        .parse::<u16>()
        .with_context(|| format!("invalid HTTP status from curl: {}", status_tail.trim()))?;

    let (headers_blob, body) = split_last_header_block(before_marker);
    let headers = parse_headers(&headers_blob);

    Ok(CurlUploadResponse {
        status,
        body,
        headers,
    })
}

fn split_last_header_block(raw: &str) -> (String, String) {
    let normalized = raw.replace("\r\n", "\n");
    let mut blocks: Vec<&str> = Vec::new();
    let mut cursor = 0;
    for (idx, _) in normalized.match_indices("\n\n") {
        blocks.push(&normalized[cursor..idx]);
        cursor = idx + 2;
    }
    let body = normalized[cursor..].to_string();
    let headers = blocks.last().map(|s| (*s).to_string()).unwrap_or_default();
    (headers, body)
}

fn parse_headers(blob: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in blob.lines() {
        if let Some((name, value)) = line.split_once(':') {
            out.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    out
}

fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Issue an HTTP request with `method` to `url` via the curl binary.
/// Retries on transient server-side failures (5xx / Cloudflare 1102).
///
/// `body` is optional JSON bytes. When provided, the request uses
/// `application/json` content type unless overridden by `extra_headers`.
pub(crate) fn request_json(
    method: &str,
    url: &str,
    body: Option<&[u8]>,
    extra_headers: &[(String, String)],
) -> Result<CurlUploadResponse> {
    let log_target = format!("{} {}", method, sanitize_url_for_log(url));
    with_retry(RetryPolicy::default_policy(), &log_target, || {
        request_json_once(method, url, body, extra_headers)
    })
}

fn request_json_once(
    method: &str,
    url: &str,
    body: Option<&[u8]>,
    extra_headers: &[(String, String)],
) -> Result<CurlUploadResponse> {
    let curl = std::env::var("ATO_CURL_BIN").unwrap_or_else(|_| "curl".to_string());

    let mut body_file_opt: Option<tempfile::NamedTempFile> = None;
    if let Some(bytes) = body {
        let mut f = tempfile::NamedTempFile::new()
            .context("Failed to create tempfile for curl request body")?;
        f.write_all(bytes)
            .context("Failed to write request body to tempfile")?;
        f.flush().context("Failed to flush request body tempfile")?;
        body_file_opt = Some(f);
    }

    let mut cmd = Command::new(&curl);
    cmd.arg("--silent")
        .arg("--show-error")
        .arg("--max-time")
        .arg("60")
        .arg("--connect-timeout")
        .arg("15")
        .arg("--request")
        .arg(method)
        .arg("--header")
        .arg("expect:")
        .arg("--dump-header")
        .arg("-")
        .arg("--write-out")
        .arg("\n__ATO_CURL_HTTP_STATUS__:%{http_code}\n");

    if let Some(f) = body_file_opt.as_ref() {
        cmd.arg("--header").arg("content-type: application/json");
        cmd.arg("--data-binary")
            .arg(format!("@{}", f.path().display()));
    }

    for (name, value) in extra_headers {
        cmd.arg("--header").arg(format!("{}: {}", name, value));
    }

    cmd.arg(url).stdout(Stdio::piped()).stderr(Stdio::piped());

    let output = cmd
        .output()
        .with_context(|| format!("Failed to launch curl ({})", curl))?;

    drop(body_file_opt);

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        bail!(
            "curl {} failed (exit={}) stderr={} stdout_tail={}",
            method,
            output.status.code().unwrap_or(-1),
            stderr.trim(),
            tail_lines(&stdout, 3)
        );
    }

    let marker = "__ATO_CURL_HTTP_STATUS__:";
    let (before_marker, status_tail) = stdout
        .rsplit_once(marker)
        .ok_or_else(|| anyhow::anyhow!("curl output did not contain HTTP status marker"))?;
    let status = status_tail
        .trim()
        .parse::<u16>()
        .with_context(|| format!("invalid HTTP status from curl: {}", status_tail.trim()))?;

    let (headers_blob, body) = split_last_header_block(before_marker);
    let headers = parse_headers(&headers_blob);

    Ok(CurlUploadResponse {
        status,
        body,
        headers,
    })
}

pub(crate) fn get(url: &str, extra_headers: &[(String, String)]) -> Result<CurlUploadResponse> {
    request_json("GET", url, None, extra_headers)
}

pub(crate) fn post_json(
    url: &str,
    json_body: &[u8],
    extra_headers: &[(String, String)],
) -> Result<CurlUploadResponse> {
    request_json("POST", url, Some(json_body), extra_headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::cell::RefCell;

    fn fast_policy(max_retries: u32) -> RetryPolicy {
        RetryPolicy {
            max_retries,
            base_delay_ms: 0,
            multiplier: 1.0,
            jitter: 0.0,
        }
    }

    fn response(status: u16, body: &str) -> CurlUploadResponse {
        CurlUploadResponse {
            status,
            body: body.to_string(),
            headers: HashMap::new(),
        }
    }

    #[test]
    fn retryable_marks_5xx_and_1102() {
        assert!(is_retryable_response(&response(500, "")));
        assert!(is_retryable_response(&response(503, "error code: 1102")));
        assert!(is_retryable_response(&response(530, "")));
        // 4xx with CF 1102 HTML page — also retryable
        assert!(is_retryable_response(&response(
            520,
            "<html>error code: 1102</html>"
        )));
        assert!(is_retryable_response(&response(
            400,
            "error code: 1102 worker exceeded"
        )));
    }

    #[test]
    fn not_retryable_on_success_or_user_4xx() {
        assert!(!is_retryable_response(&response(200, "ok")));
        // A 200 payload containing the literal text is NOT retryable — only
        // failure statuses trigger the 1102 marker path.
        assert!(!is_retryable_response(&response(200, "error code: 1102")));
        assert!(!is_retryable_response(&response(404, "not found")));
        assert!(!is_retryable_response(&response(409, "already exists")));
        assert!(!is_retryable_response(&response(413, "too large")));
    }

    #[test]
    fn with_retry_returns_first_success() {
        let calls = RefCell::new(0);
        let out = with_retry(fast_policy(3), "test", || {
            *calls.borrow_mut() += 1;
            Ok(response(200, "ok"))
        })
        .expect("ok");
        assert_eq!(out.status, 200);
        assert_eq!(*calls.borrow(), 1);
    }

    #[test]
    fn with_retry_eventually_succeeds_after_5xx() {
        let calls = RefCell::new(0);
        let out = with_retry(fast_policy(3), "test", || {
            let n = {
                let mut c = calls.borrow_mut();
                *c += 1;
                *c
            };
            if n < 3 {
                Ok(response(503, "fail"))
            } else {
                Ok(response(200, "ok"))
            }
        })
        .expect("ok");
        assert_eq!(out.status, 200);
        assert_eq!(*calls.borrow(), 3);
    }

    #[test]
    fn with_retry_gives_up_after_max_attempts_and_returns_last_response() {
        let calls = RefCell::new(0);
        let out = with_retry(fast_policy(2), "test", || {
            *calls.borrow_mut() += 1;
            Ok(response(503, "still failing"))
        })
        .expect("ok");
        assert_eq!(out.status, 503);
        // 1 initial + 2 retries
        assert_eq!(*calls.borrow(), 3);
    }

    #[test]
    fn with_retry_returns_immediately_on_4xx() {
        let calls = RefCell::new(0);
        let out = with_retry(fast_policy(3), "test", || {
            *calls.borrow_mut() += 1;
            Ok(response(404, "not found"))
        })
        .expect("ok");
        assert_eq!(out.status, 404);
        assert_eq!(*calls.borrow(), 1);
    }

    #[test]
    fn with_retry_retries_transport_errors() {
        let calls = RefCell::new(0);
        let out = with_retry(fast_policy(2), "test", || {
            let n = {
                let mut c = calls.borrow_mut();
                *c += 1;
                *c
            };
            if n < 3 {
                Err(anyhow::anyhow!("curl exited nonzero"))
            } else {
                Ok(response(200, "ok"))
            }
        })
        .expect("ok");
        assert_eq!(out.status, 200);
        assert_eq!(*calls.borrow(), 3);
    }

    #[test]
    fn with_retry_retries_on_cloudflare_1102_body() {
        let calls = RefCell::new(0);
        let out = with_retry(fast_policy(2), "test", || {
            let n = {
                let mut c = calls.borrow_mut();
                *c += 1;
                *c
            };
            if n < 2 {
                Ok(response(520, "<html>error code: 1102</html>"))
            } else {
                Ok(response(200, "ok"))
            }
        })
        .expect("ok");
        assert_eq!(out.status, 200);
        assert_eq!(*calls.borrow(), 2);
    }

    #[test]
    #[serial]
    fn with_retry_disabled_via_env_skips_retries() {
        std::env::set_var("ATO_DISABLE_RETRY", "1");
        let calls = RefCell::new(0);
        let out = with_retry(fast_policy(3), "test", || {
            *calls.borrow_mut() += 1;
            Ok(response(503, "fail"))
        })
        .expect("ok");
        std::env::remove_var("ATO_DISABLE_RETRY");
        assert_eq!(out.status, 503);
        assert_eq!(*calls.borrow(), 1);
    }

    #[test]
    fn sanitize_url_strips_query_string() {
        assert_eq!(
            sanitize_url_for_log("https://api.ato.run/v1/capsules/abc?x-amz-signature=secret"),
            "https://api.ato.run/v1/capsules/abc"
        );
        assert_eq!(
            sanitize_url_for_log("https://api.ato.run/v1/capsules"),
            "https://api.ato.run/v1/capsules"
        );
    }
}
