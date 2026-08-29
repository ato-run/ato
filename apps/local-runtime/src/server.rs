//! Loopback HTTP transport.
//!
//! Hand-rolled rather than framework-backed: the runtime serves three routes on
//! loopback, and a shipped desktop artifact has less to audit for every
//! dependency it does not take. Nothing here decides anything about execution —
//! each handler translates a request, calls `ato-local-execution`, and
//! translates the result back.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ato_local_execution::authoring::{initial_computation, load_config};
use ato_local_execution::{core_materializer_registry, start_durable, stop_and_seal};
use ato_objects::LocalCapsuleRepository;

use crate::protocol::{ErrorBody, ExecutionView, ProjectRequest, StartRequest};

pub struct Server {
    listener: TcpListener,
    work_root: PathBuf,
    credential: String,
}

impl Server {
    /// Bind loopback on an OS-assigned port.
    ///
    /// Loopback only, never 0.0.0.0: this runtime executes arbitrary local
    /// Computations on behalf of one machine's own user, and must not be
    /// reachable from the network under any configuration.
    pub fn bind(work_root: PathBuf, credential: String) -> Result<Self> {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .context("binding a loopback port")?;
        Ok(Self {
            listener,
            work_root,
            credential,
        })
    }

    pub fn port(&self) -> u16 {
        self.listener
            .local_addr()
            .map(|address| address.port())
            .unwrap_or(0)
    }

    pub fn serve(&self) -> Result<()> {
        for stream in self.listener.incoming() {
            match stream {
                Ok(stream) => {
                    if let Err(error) = self.handle(stream) {
                        // One bad request must never take the runtime down.
                        eprintln!("ato-local-runtime: request failed: {error:#}");
                    }
                }
                Err(error) => eprintln!("ato-local-runtime: accept failed: {error}"),
            }
        }
        Ok(())
    }

    fn handle(&self, mut stream: TcpStream) -> Result<()> {
        let request = read_request(&mut stream)?;

        if !authorized(&request, &self.credential) {
            return respond(
                &mut stream,
                401,
                &ErrorBody {
                    error: "unauthorized".into(),
                },
            );
        }

        let outcome = match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/v1/executions") => self.start(&request.body),
            ("POST", "/v1/executions/stop") => self.stop(&request.body),
            ("POST", "/v1/executions/status") => self.status(&request.body),
            _ => {
                return respond(
                    &mut stream,
                    404,
                    &ErrorBody {
                        error: "not_found".into(),
                    },
                );
            }
        };

        match outcome {
            Ok(view) => respond(&mut stream, 200, &view),
            Err(error) => respond(
                &mut stream,
                400,
                &ErrorBody {
                    error: format!("{error:#}"),
                },
            ),
        }
    }

    /// Start (or adopt) a durable execution for a project.
    ///
    /// The only Ato decision made here is "has this project been initialized
    /// yet"; everything after is the library's.
    fn start(&self, body: &str) -> Result<ExecutionView> {
        let request: StartRequest = serde_json::from_str(body).context("invalid start request")?;
        let project = self.resolve(&request.project)?;
        let repository = LocalCapsuleRepository::open(&project)?;

        let head = match repository.head("main")? {
            Some(head) => head,
            None => {
                let config = load_config(&project)?;
                let initial = initial_computation(&repository, config)?;
                repository.create_branch("main", &initial, None)?;
                initial
            }
        };

        let bindings: BTreeMap<String, String> = request.bindings;
        start_durable(
            &repository,
            "main",
            &head,
            &bindings,
            None,
            &core_materializer_registry,
        )?;
        view(&repository, &project)
    }

    fn stop(&self, body: &str) -> Result<ExecutionView> {
        let request: ProjectRequest = serde_json::from_str(body).context("invalid stop request")?;
        let project = self.resolve(&request.project)?;
        let repository = LocalCapsuleRepository::open(&project)?;
        // The full seal, not just the quiesce: stopping is five steps, and
        // `evolve_workspace` inside them is where the head actually moves.
        let sealed = stop_and_seal(&repository)?;
        let mut view = view(&repository, &project)?;
        if let Some(sealed) = sealed {
            view.execution_id = sealed.run.token;
            view.head = sealed.head.to_string();
            view.status = "sealed".to_owned();
            view.pid = None;
        }
        Ok(view)
    }

    fn status(&self, body: &str) -> Result<ExecutionView> {
        let request: ProjectRequest =
            serde_json::from_str(body).context("invalid status request")?;
        let project = self.resolve(&request.project)?;
        let repository = LocalCapsuleRepository::open(&project)?;
        view(&repository, &project)
    }

    /// Resolve a caller-supplied project path inside the work root.
    ///
    /// Refuses anything that escapes it. A caller that can name an arbitrary
    /// path could make the runtime open a repository anywhere on the machine,
    /// which is not what "execute my project" should be able to mean.
    fn resolve(&self, project: &str) -> Result<PathBuf> {
        let candidate = Path::new(project);
        let resolved = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.work_root.join(candidate)
        };
        let root = self
            .work_root
            .canonicalize()
            .unwrap_or_else(|_| self.work_root.clone());
        let resolved_canonical = resolved.canonicalize().unwrap_or_else(|_| resolved.clone());
        anyhow::ensure!(
            resolved_canonical.starts_with(&root),
            "project path escapes the work root"
        );
        Ok(resolved_canonical)
    }
}

/// Project the repository's own state; nothing is computed or inferred here.
fn view(repository: &LocalCapsuleRepository, project: &Path) -> Result<ExecutionView> {
    let active = repository.active_run()?;
    let head = repository
        .head("main")?
        .map(|head| head.to_string())
        .unwrap_or_default();
    Ok(match active {
        Some(run) => ExecutionView {
            execution_id: run.token.clone(),
            project: project.display().to_string(),
            branch: run.branch.clone(),
            head: run.head.to_string(),
            record_seq: run.record_seq,
            status: run.status.clone(),
            pid: (run.pid != 0).then_some(run.pid),
        },
        None => ExecutionView {
            execution_id: String::new(),
            project: project.display().to_string(),
            branch: "main".to_owned(),
            head,
            record_seq: 0,
            status: "idle".to_owned(),
            pid: None,
        },
    })
}

struct Request {
    method: String,
    path: String,
    authorization: Option<String>,
    body: String,
}

fn read_request(stream: &mut TcpStream) -> Result<Request> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();

    let mut authorization = None;
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
        let lowered = header.to_ascii_lowercase();
        if let Some(value) = lowered.strip_prefix("authorization:") {
            authorization = Some(
                header[header.find(':').unwrap_or(0) + 1..]
                    .trim()
                    .to_owned(),
            );
            let _ = value;
        } else if let Some(value) = lowered.strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body)?;
    }
    Ok(Request {
        method,
        path,
        authorization,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

/// Constant-time credential check.
///
/// A short-circuiting comparison leaks how much of a guess was correct, which
/// is exactly what a local attacker probing a loopback port would measure.
fn authorized(request: &Request, credential: &str) -> bool {
    let Some(header) = request.authorization.as_deref() else {
        return false;
    };
    let Some(presented) = header.strip_prefix("Bearer ") else {
        return false;
    };
    constant_time_eq(presented.as_bytes(), credential.as_bytes())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right) {
        difference |= a ^ b;
    }
    difference == 0
}

fn respond<T: serde::Serialize>(stream: &mut TcpStream, status: u16, body: &T) -> Result<()> {
    let payload = serde_json::to_string(body)?;
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        _ => "Not Found",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    )?;
    stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_compared_in_constant_time_and_by_value() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        // Length mismatch must not be treated as a prefix match.
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"a"));
    }

    fn request(authorization: Option<&str>) -> Request {
        Request {
            method: "POST".into(),
            path: "/v1/executions".into(),
            authorization: authorization.map(str::to_owned),
            body: String::new(),
        }
    }

    #[test]
    fn only_the_exact_bearer_credential_is_accepted() {
        let credential = "c".repeat(64);
        assert!(authorized(
            &request(Some(&format!("Bearer {credential}"))),
            &credential
        ));
        assert!(!authorized(&request(Some("Bearer wrong")), &credential));
        // A credential presented without the scheme is not a credential.
        assert!(!authorized(&request(Some(&credential)), &credential));
        assert!(!authorized(&request(None), &credential));
    }

    #[test]
    fn a_project_path_cannot_escape_the_work_root() {
        // Otherwise "execute my project" could mean "open a repository
        // anywhere on this machine".
        let root = tempfile::tempdir().expect("work root");
        std::fs::create_dir_all(root.path().join("inside")).unwrap();
        let server = Server {
            listener: TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap(),
            work_root: root.path().to_path_buf(),
            credential: "x".repeat(64),
        };
        assert!(server.resolve("inside").is_ok());
        assert!(server.resolve("../../etc").is_err());
        assert!(server.resolve("/etc").is_err());
    }

    #[test]
    fn the_listener_is_loopback_only() {
        let root = tempfile::tempdir().expect("work root");
        let server = Server::bind(root.path().to_path_buf(), "y".repeat(64)).unwrap();
        let address = server.listener.local_addr().unwrap();
        assert!(address.ip().is_loopback(), "bound {address}");
        assert_ne!(server.port(), 0);
    }
}
