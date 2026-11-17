//! Serve a local MLX model as Ato's System One generation endpoint.
//!
//! The generation contract is a choice task, so a local instruct model can answer
//! it: the request carries its own instructions, and the runner uses them verbatim
//! so the prompt cannot drift from the contract. This is what makes
//! "the user's own API or a local model" selectable without a cloud account:
//!
//!   ato llm serve --model <path-or-hub-id> --port 8099
//!   ATO_GENERATION_JEV_BASE_URL=http://127.0.0.1:8099 \
//!     ato form <dir> --generation-provider jev ...
//!
//! The forward pass lives in an embedded runner executed as a separate process, so
//! a model crash or an out-of-memory kill cannot take the CLI down.
use std::io::{BufRead as _, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use serde::Serialize;

const LLM_RUNNER: &str = include_str!("../assets/llm_runner.py");
const SERVE_SCHEMA: &str = "ato.llm-serve/1";
/// The reference runtime's request ceiling for a state plus its questions.
const MAX_REQUEST_BYTES: usize = 48 * 1024;

#[derive(Args)]
pub struct ServeArgs {
    /// Local model directory or Hub id the MLX runtime can load.
    #[arg(long)]
    pub model: String,
    /// Interface to bind. Keep the default: this endpoint has no rate limit.
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
    /// Port to bind.
    #[arg(long, default_value_t = 8099)]
    pub port: u16,
    /// Where the runner script is written. Defaults to ATO_HOME/llm.
    #[arg(long)]
    pub home: Option<PathBuf>,
    /// Interpreter that has mlx-lm installed. Defaults to ATO_LLM_PYTHON, then python3.
    #[arg(long)]
    pub python: Option<PathBuf>,
    /// Print the machine-readable startup line only.
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize)]
pub struct ServeReport {
    pub schema: &'static str,
    pub model: String,
    pub endpoint: String,
    pub health: String,
    pub systemone: String,
}

pub fn serve(args: &ServeArgs) -> Result<()> {
    let root = match &args.home {
        Some(home) => home.join("llm"),
        None => super::ato_home()?.join("llm"),
    };
    std::fs::create_dir_all(&root).with_context(|| format!("create {}", root.display()))?;
    let runner = root.join("llm_runner.py");
    std::fs::write(&runner, LLM_RUNNER).with_context(|| format!("write {}", runner.display()))?;
    let python = args
        .python
        .clone()
        .or_else(|| std::env::var_os("ATO_LLM_PYTHON").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("python3"));
    let listener = std::net::TcpListener::bind((args.host.as_str(), args.port))
        .with_context(|| format!("listen on {}:{}", args.host, args.port))?;
    let address = listener.local_addr().context("read the bound address")?;
    let endpoint = format!("http://{address}");
    let report = ServeReport {
        schema: SERVE_SCHEMA,
        model: args.model.clone(),
        endpoint: endpoint.clone(),
        health: format!("{endpoint}/healthz"),
        systemone: format!("{endpoint}/v1/systemone"),
    };
    if args.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("serving {} on {endpoint}", args.model);
        println!("  health:     {endpoint}/healthz");
        println!("  system one: {endpoint}/v1/systemone");
        println!("  point a generation provider at it with ATO_GENERATION_JEV_BASE_URL={endpoint}");
        println!("  stop with Ctrl-C");
    }
    let mut worker: Option<RunnerWorker> = None;
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let _ = handle_connection(&mut stream, &python, &runner, &args.model, &mut worker);
    }
    Ok(())
}

/// One long-lived runner process: the model is loaded for the first request only.
struct RunnerWorker {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: std::io::BufReader<std::process::ChildStdout>,
}

impl RunnerWorker {
    fn spawn(python: &Path, runner: &Path, model: &str) -> Result<Self> {
        let mut child = std::process::Command::new(python)
            .arg(runner)
            .arg(model)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .with_context(|| format!("start {}", python.display()))?;
        let stdin = child.stdin.take().context("runner stdin is unavailable")?;
        let stdout = std::io::BufReader::new(
            child
                .stdout
                .take()
                .context("runner stdout is unavailable")?,
        );
        Ok(Self {
            child,
            stdin,
            stdout,
        })
    }

    fn ask(&mut self, request: &serde_json::Value) -> Result<serde_json::Value> {
        writeln!(self.stdin, "{}", serde_json::to_string(request)?)
            .context("write the runner request")?;
        self.stdin.flush().context("flush the runner request")?;
        let mut line = String::new();
        let read = self
            .stdout
            .read_line(&mut line)
            .context("read the runner response")?;
        ensure!(read > 0, "the runner closed its output");
        let value: serde_json::Value =
            serde_json::from_str(line.trim()).context("the runner response is not JSON")?;
        if let Some(error) = value.get("error") {
            bail!(
                "{}: {}",
                error
                    .get("code")
                    .and_then(|code| code.as_str())
                    .unwrap_or("llm_runtime_failed"),
                error
                    .get("message")
                    .and_then(|message| message.as_str())
                    .unwrap_or("the runner reported an error")
            );
        }
        Ok(value)
    }
}

impl Drop for RunnerWorker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn handle_connection(
    stream: &mut std::net::TcpStream,
    python: &Path,
    runner: &Path,
    model: &str,
    worker: &mut Option<RunnerWorker>,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    let mut reader = std::io::BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
        if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let (status, body) = match (method.as_str(), path.as_str()) {
        ("GET", "/healthz") => (
            "200 OK",
            serde_json::json!({"status": "ok", "model": model}).to_string(),
        ),
        ("POST", "/v1/systemone") => {
            let mut body = vec![0u8; length.min(MAX_REQUEST_BYTES)];
            reader.read_exact(&mut body)?;
            if length > MAX_REQUEST_BYTES {
                ("413 Payload Too Large", error_body("request is too large"))
            } else {
                match systemone(python, runner, model, &body, worker) {
                    Ok(answer) => ("200 OK", answer),
                    Err(error) => ("422 Unprocessable Entity", error_body(&error.to_string())),
                }
            }
        }
        _ => ("404 Not Found", error_body("unknown route")),
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()?;
    Ok(())
}

fn error_body(message: &str) -> String {
    serde_json::json!({"error": {"code": "invalid", "message": message}}).to_string()
}

fn systemone(
    python: &Path,
    runner: &Path,
    model: &str,
    body: &[u8],
    worker: &mut Option<RunnerWorker>,
) -> Result<String> {
    let request: serde_json::Value =
        serde_json::from_slice(body).context("the request is not valid JSON")?;
    let questions = request
        .get("questions")
        .cloned()
        .context("the request needs questions")?;
    ensure!(
        questions
            .as_object()
            .map(|map| !map.is_empty())
            .unwrap_or(false),
        "the request needs at least one question"
    );
    let payload = serde_json::json!({
        "model": model,
        "state": request.get("state").cloned().unwrap_or(serde_json::Value::Null),
        "questions": questions,
    });
    let value = match ask_worker(python, runner, model, &payload, worker) {
        Ok(value) => value,
        Err(_) => {
            // A worker that died or answered garbage is replaced once.
            *worker = None;
            ask_worker(python, runner, model, &payload, worker)?
        }
    };
    // The generation contract requires the response to name the model the caller
    // asked for; the local model is a server detail, reported by /healthz.
    let requested = request
        .get("model")
        .and_then(|value| value.as_str())
        .unwrap_or(model);
    Ok(serde_json::json!({
        "model": requested,
        "answers": value.get("answers").cloned().unwrap_or(serde_json::Value::Null),
        "usage": value.get("usage").cloned().unwrap_or(serde_json::Value::Null),
    })
    .to_string())
}

fn ask_worker(
    python: &Path,
    runner: &Path,
    model: &str,
    payload: &serde_json::Value,
    worker: &mut Option<RunnerWorker>,
) -> Result<serde_json::Value> {
    if worker.is_none() {
        *worker = Some(RunnerWorker::spawn(python, runner, model)?);
    }
    let running = worker.as_mut().context("the runner is not running")?;
    running.ask(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A counter, not a timestamp: two tests starting in the same clock tick would
    /// otherwise share a directory and one would rewrite the other's stub mid-run.
    fn temp_root() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".tmp/llm-serve");
        std::fs::create_dir_all(&base).expect("test root");
        let root = base.join(format!(
            "run-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&root).expect("root");
        root
    }

    /// A line-protocol stub that counts the requests one process served.
    fn write_stub(root: &Path) -> PathBuf {
        let script = root.join("stub_runner.py");
        let stub = concat!(
            "import json, sys\n",
            "seen = 0\n",
            "for line in sys.stdin:\n",
            "    if not line.strip():\n",
            "        continue\n",
            "    request = json.loads(line)\n",
            "    seen += 1\n",
            "    json.dump({'answers': {'operation': {'type': 'choice', 'choice': 'python_script',\n",
            "                                         'confidence': 0.9,\n",
            "                                         'probabilities': {'python_script': 0.9, 'decline': 0.1}},\n",
            "                          'entrypoint': {'type': 'choice', 'choice': 'ep_A1',\n",
            "                                         'confidence': 0.9,\n",
            "                                         'probabilities': {'ep_A1': 0.9, 'none': 0.1}},\n",
            "                          'seen': seen},\n",
            "               'usage': {'input_tokens': 10, 'output_tokens': 5}}, sys.stdout)\n",
            "    sys.stdout.write('\\n')\n",
            "    sys.stdout.flush()\n",
        );
        std::fs::write(&script, stub).expect("stub");
        script
    }

    fn exchange(
        python: &Path,
        runner: &Path,
        model: &str,
        request: &str,
        worker: &mut Option<RunnerWorker>,
    ) -> String {
        use std::io::Read as _;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("addr");
        let python = python.to_path_buf();
        let runner = runner.to_path_buf();
        let model = model.to_owned();
        let mut client = std::net::TcpStream::connect(address).expect("connect");
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let (mut stream, _) = listener.accept().expect("accept");
                handle_connection(&mut stream, &python, &runner, &model, worker).expect("handle");
            });
            client.write_all(request.as_bytes()).expect("write");
            client.flush().expect("flush");
            let mut response = String::new();
            client.read_to_string(&mut response).expect("read");
            response
        })
    }

    fn systemone_request(model: &str) -> String {
        let body = format!(
            "{{\"model\":\"{model}\",\"state\":{{\"entrypoint_ids\":[\"ep_A1\"]}},\"questions\":{{\"operation\":{{\"type\":\"choice\"}}}}}}"
        );
        format!(
            "POST /v1/systemone HTTP/1.1\r\nhost: localhost\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn health_answers_without_starting_the_model() {
        let root = temp_root();
        let response = exchange(
            Path::new("/nonexistent/python"),
            Path::new("/nonexistent/runner.py"),
            "local-model",
            "GET /healthz HTTP/1.1\r\nhost: localhost\r\n\r\n",
            &mut None,
        );
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert!(response.contains("\"model\":\"local-model\""), "{response}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn an_unknown_route_is_a_404() {
        let root = temp_root();
        let response = exchange(
            Path::new("/nonexistent/python"),
            Path::new("/nonexistent/runner.py"),
            "local-model",
            "GET /nope HTTP/1.1\r\nhost: localhost\r\n\r\n",
            &mut None,
        );
        assert!(response.starts_with("HTTP/1.1 404 Not Found"), "{response}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_request_without_questions_is_a_422() {
        let root = temp_root();
        let body = "{\"state\":{},\"questions\":{}}";
        let request = format!(
            "POST /v1/systemone HTTP/1.1\r\nhost: localhost\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        );
        let response = exchange(
            Path::new("/nonexistent/python"),
            Path::new("/nonexistent/runner.py"),
            "local-model",
            &request,
            &mut None,
        );
        assert!(
            response.starts_with("HTTP/1.1 422 Unprocessable Entity"),
            "{response}"
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// The generation contract compares the response's model with the one it asked
    /// for, so the server must echo the caller's identity, not the local path.
    #[test]
    fn the_served_model_is_the_one_the_caller_asked_for() {
        let root = temp_root();
        let script = write_stub(&root);
        let response = exchange(
            Path::new("python3"),
            &script,
            "/local/model/path",
            &systemone_request("jev-1.13.0"),
            &mut None,
        );
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert!(response.contains("\"model\":\"jev-1.13.0\""), "{response}");
        assert!(
            response.contains("\"choice\":\"python_script\""),
            "{response}"
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// Two requests must share one interpreter: the model load is the expensive part.
    #[test]
    fn two_requests_share_one_interpreter() {
        let root = temp_root();
        let script = write_stub(&root);
        let mut worker = None;
        let first = exchange(
            Path::new("python3"),
            &script,
            "local",
            &systemone_request("jev-1.13.0"),
            &mut worker,
        );
        assert!(first.contains("\"seen\":1"), "{first}");
        let second = exchange(
            Path::new("python3"),
            &script,
            "local",
            &systemone_request("jev-1.13.0"),
            &mut worker,
        );
        assert!(
            second.contains("\"seen\":2"),
            "the second request must reach the same interpreter: {second}"
        );
        std::fs::remove_dir_all(root).ok();
    }
}
