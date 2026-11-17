//! `ato ui`: a loopback-only management page for the local Instance library.
//!
//! The page is a thin front-end over the same CLI: every endpoint runs this
//! binary's own subcommands and returns their output. That keeps one authority
//! for state and lifecycle (the CLI) instead of a second server-side model.
//!
//! It binds 127.0.0.1 only and has no authentication: the threat model is the
//! same as the user's own terminal. It is not a network service.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use std::process::Command;

use anyhow::{Context, Result, bail};

const PAGE: &str = r#"<!doctype html>
<html lang="ja"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Ato</title>
<style>
 body{font:14px/1.5 -apple-system,system-ui,sans-serif;margin:0;background:#0f1115;color:#e6e8ec}
 header{padding:20px 24px;border-bottom:1px solid #23262d;display:flex;gap:12px;align-items:baseline}
 h1{font-size:16px;margin:0;font-weight:600}
 .muted{color:#8b93a1;font-size:12px}
 main{padding:16px 24px;max-width:960px}
 .row{border:1px solid #23262d;border-radius:8px;padding:12px 14px;margin-bottom:10px;display:flex;gap:14px;align-items:center}
 .row .id{font-family:ui-monospace,monospace;font-size:12px;color:#9aa4b2}
 .state{font-size:12px;padding:2px 8px;border-radius:999px;background:#1c2028;border:1px solid #2b303a}
 .state.running{background:#0f2e1d;border-color:#1d6b42;color:#7ee2a8}
 .state.stopped{color:#9aa4b2}
 .grow{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
 button{font:inherit;background:#23262d;color:#e6e8ec;border:1px solid #343943;border-radius:6px;padding:5px 12px;cursor:pointer}
 button.primary{background:#2b5cff;border-color:#2b5cff}
 button:disabled{opacity:.4;cursor:default}
 pre{background:#151820;border:1px solid #23262d;border-radius:8px;padding:12px;overflow:auto;font-size:12px}
</style></head><body>
<header><h1>Ato</h1><span class="muted" id="count"></span><span class="grow"></span>
<label class="muted"><input type="checkbox" id="auto" checked> 5秒ごとに更新</label></header>
<main>
<div id="list"><span class="muted">読み込み中…</span></div>
<h3 style="font-size:13px;margin:24px 0 8px" class="muted">操作ログ</h3>
<pre id="log">-</pre>
</main>
<script>
const log = (m) => { const el = document.getElementById('log'); el.textContent = m + "\n" + el.textContent; };
async function call(path, method) {
  const r = await fetch(path, {method: method || 'GET'});
  const text = await r.text();
  if (!r.ok) throw new Error(text || r.status);
  return text;
}
function row(entry) {
  const i = entry.instance, state = entry.state || 'stopped';
  const run = entry.active_run;
  const div = document.createElement('div');
  div.className = 'row';
  div.innerHTML = '<span class="state ' + state + '">' + state + '</span>' +
    '<span class="grow"><span class="id">' + i.instance_id + '</span><br>' +
    '<span class="muted">' + i.contract_ref + (run && run.url ? ' · ' + run.url : '') + '</span></span>';
  const start = document.createElement('button');
  start.textContent = '起動'; start.className = 'primary'; start.disabled = state === 'running';
  start.onclick = async () => { try { log(await call('/api/instances/' + i.instance_id + '/start', 'POST')); } catch (e) { log('失敗: ' + e.message); } refresh(); };
  const stop = document.createElement('button');
  stop.textContent = '停止'; stop.disabled = state !== 'running' && state !== 'starting';
  stop.onclick = async () => { try { log(await call('/api/instances/' + i.instance_id + '/stop', 'POST')); } catch (e) { log('失敗: ' + e.message); } refresh(); };
  div.append(start, stop);
  return div;
}
async function refresh() {
  try {
    const doc = JSON.parse(await call('/api/instances'));
    const list = document.getElementById('list');
    list.textContent = '';
    document.getElementById('count').textContent = doc.instances.length + ' 件';
    if (!doc.instances.length) { list.innerHTML = '<span class="muted">ローカル Instance はありません。ato app add で追加してください。</span>'; return; }
    doc.instances.forEach(e => list.append(row(e)));
  } catch (e) { document.getElementById('list').textContent = '読み込み失敗: ' + e.message; }
}
document.getElementById('auto').onchange = (e) => { if (e.target.checked) setInterval(refresh, 5000); };
refresh();
</script></body></html>"#;

#[derive(Debug, clap::Args)]
pub struct UiArgs {
    /// Port to bind on 127.0.0.1. Omit for an ephemeral port.
    #[arg(long)]
    pub port: Option<u16>,
    /// Print the URL without opening a browser.
    #[arg(long)]
    pub no_open: bool,
}

struct Request {
    method: String,
    path: String,
}

fn parse_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buffer = [0u8; 8192];
    let mut filled = 0usize;
    loop {
        let count = stream.read(&mut buffer[filled..]).ok()?;
        if count == 0 {
            return None;
        }
        filled += count;
        if buffer[..filled]
            .windows(4)
            .any(|window| window == b"\r\n\r\n")
        {
            break;
        }
        if filled == buffer.len() {
            return None;
        }
    }
    let head = String::from_utf8_lossy(&buffer[..filled]);
    let mut request_line = head.lines().next()?.split_whitespace();
    let method = request_line.next()?.to_owned();
    let target = request_line.next()?.to_owned();
    let path = target.split('?').next().unwrap_or("/").to_owned();
    Some(Request { method, path })
}

fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn run_ato(args: &[&str]) -> Result<String> {
    let exe = std::env::current_exe().context("this binary has no path")?;
    let output = Command::new(exe)
        .args(args)
        .output()
        .with_context(|| format!("run ato {}", args.join(" ")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        bail!("{stderr}");
    }
}

fn instances_json() -> Result<Vec<u8>> {
    Ok(run_ato(&["app", "list", "--json"])?.into_bytes())
}

fn handle(request: &Request) -> (String, &'static str, Vec<u8>) {
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => (
            "200 OK".to_owned(),
            "text/html; charset=utf-8",
            PAGE.as_bytes().to_vec(),
        ),
        ("GET", "/api/instances") => match instances_json() {
            Ok(body) => ("200 OK".to_owned(), "application/json", body),
            Err(error) => (
                "500 Internal Server Error".to_owned(),
                "text/plain; charset=utf-8",
                error.to_string().into_bytes(),
            ),
        },
        (method, path) if method == "POST" && path.starts_with("/api/instances/") => {
            let rest = path.trim_start_matches("/api/instances/");
            let Some((instance_id, action)) = rest.rsplit_once('/') else {
                return (
                    "400 Bad Request".to_owned(),
                    "text/plain; charset=utf-8",
                    b"missing action".to_vec(),
                );
            };
            if instance_id.is_empty() || instance_id.contains('/') {
                return (
                    "400 Bad Request".to_owned(),
                    "text/plain; charset=utf-8",
                    b"bad instance id".to_vec(),
                );
            }
            let result = match action {
                "start" => run_ato(&["app", "start", instance_id, "--no-open"]),
                "stop" => run_ato(&["app", "stop", instance_id]),
                "remove" => run_ato(&["app", "remove", instance_id]),
                _ => {
                    return (
                        "404 Not Found".to_owned(),
                        "text/plain; charset=utf-8",
                        b"unknown action".to_vec(),
                    );
                }
            };
            match result {
                Ok(stdout) => (
                    "200 OK".to_owned(),
                    "text/plain; charset=utf-8",
                    stdout.into_bytes(),
                ),
                Err(error) => (
                    "400 Bad Request".to_owned(),
                    "text/plain; charset=utf-8",
                    error.to_string().into_bytes(),
                ),
            }
        }
        _ => (
            "404 Not Found".to_owned(),
            "text/plain; charset=utf-8",
            b"not found".to_vec(),
        ),
    }
}

pub fn serve(args: UiArgs) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", args.port.unwrap_or(0)))
        .context("cannot bind 127.0.0.1")?;
    let address = listener.local_addr()?;
    let url = format!("http://{address}/");
    println!("{url}");
    if !args.no_open {
        let _ = open_browser_quiet(&url);
    }
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        std::thread::spawn(move || {
            let Some(request) = parse_request(&mut stream) else {
                return;
            };
            let (status, content_type, body) = handle(&request);
            respond(&mut stream, &status, content_type, &body);
        });
    }
    Ok(())
}

fn open_browser_quiet(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(not(target_os = "macos"))]
    let mut command = Command::new("xdg-open");
    command.arg(url).status().context("open the browser")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_is_self_contained() {
        assert!(PAGE.starts_with("<!doctype html>"));
        assert!(PAGE.contains("fetch('/api/instances')") || PAGE.contains("/api/instances"));
        assert!(!PAGE.contains("cdn.") && !PAGE.contains("src=\"http"));
    }

    #[test]
    fn routes_answer_without_touching_the_library() {
        let (status, content_type, body) = handle(&Request {
            method: "GET".into(),
            path: "/".into(),
        });
        assert_eq!(status, "200 OK");
        assert_eq!(content_type, "text/html; charset=utf-8");
        assert!(body.starts_with(b"<!doctype html>"));
        let (status, _, _) = handle(&Request {
            method: "GET".into(),
            path: "/nope".into(),
        });
        assert_eq!(status, "404 Not Found");
    }
}
