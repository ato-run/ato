//! A scoped CandidateProducer bridge. It exposes saved public input, never the
//! owner journal, API token, source workspace, Runtime ticket or receipt body.
use super::*;
use std::{
    io::{BufRead, BufReader},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
};

pub const CONNECTION_SCHEMA: &str = "ato.formation-session-connection/1";
pub const VIEW_SCHEMA: &str = "ato.formation-session-view/1";
const FRAME_CAP: usize = 96 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub schema: String,
    pub address: SocketAddr,
    pub access_token: String,
    pub search_id: String,
    pub configuration_ref: String,
    pub agent: Option<SessionAgent>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Next,
    Status,
    Submit {
        exchange_id: String,
        input_sha256: String,
        output: Box<serde_json::value::RawValue>,
    },
    Cancel,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    access_token: String,
    search_id: String,
    configuration_ref: String,
    command: Command,
}

/// Only the trusted requester creates/reconnects this server. The capability in
/// `connection_file` grants these four operations for one frozen Search only.
#[derive(Clone, Copy)]
pub struct ResponseWindow {
    pub deadline_ms: u64,
    pub now_ms: u64,
    pub accepting: bool,
}

pub struct Bridge {
    connection: Connection,
    connection_file: PathBuf,
    stop: Arc<AtomicBool>,
    progress: Arc<Mutex<Value>>,
    producer: Arc<ReasoningProducer>,
    deadline_ms: u64,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Bridge {
    pub fn start(
        producer: Arc<ReasoningProducer>,
        search_id: &str,
        deadline_ms: u64,
        connection_file: &Path,
        entropy: [u8; 32],
        cancel: Arc<dyn Fn() -> Result<Value> + Send + Sync>,
    ) -> Result<Self> {
        ensure!(
            producer.config.is_session(),
            "bridge requires a Session producer"
        );
        let agent = match &producer.config {
            ReasoningProviderConfig::Session(c) => c.agent.clone(),
            _ => None,
        };
        let mut connection = Connection {
            schema: CONNECTION_SCHEMA.into(),
            address: "127.0.0.1:0".parse()?,
            access_token: BASE64.encode(entropy),
            search_id: search_id.into(),
            configuration_ref: producer.configuration_ref.clone(),
            agent,
        };
        if connection_file.exists() {
            let saved = read_connection(connection_file)?;
            ensure!(
                saved.search_id == connection.search_id
                    && saved.configuration_ref == connection.configuration_ref
                    && saved.agent == connection.agent,
                "session connection binding changed"
            );
            connection.access_token = saved.access_token;
        }
        let listener = TcpListener::bind(connection.address)?;
        listener.set_nonblocking(true)?;
        connection.address = listener.local_addr()?;
        let progress = Arc::new(Mutex::new(json!({"status":"connecting"})));
        let stop = Arc::new(AtomicBool::new(false));
        if producer.session_cancelled.load(Ordering::Relaxed) {
            producer.request_session_cancellation(search_id)?;
            let reconciled = cancel()?;
            *progress
                .lock()
                .map_err(|_| anyhow::anyhow!("session state unavailable"))? =
                public_progress(&reconciled);
        }
        let (state, stopped, identity) = (progress.clone(), stop.clone(), connection.clone());
        let server_producer = producer.clone();
        let thread = std::thread::spawn(move || {
            let producer = server_producer;
            while !stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let answer = (|| -> Result<Value> {
                            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                            stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                            let raw = read_frame(&mut stream)?;
                            let request: Request = serde_json::from_slice(&raw)?;
                            // Do not echo parser errors or attacker-controlled request bytes.
                            ensure!(
                                same_token(&request.access_token, &identity.access_token)
                                    && request.search_id == identity.search_id
                                    && request.configuration_ref == identity.configuration_ref,
                                "session authentication rejected"
                            );
                            let now = now_ms()?;
                            match request.command {
                                Command::Next | Command::Status => producer.session_view(
                                    &identity.search_id,
                                    deadline_ms,
                                    now,
                                    &*state.lock().map_err(|_| {
                                        anyhow::anyhow!("session state unavailable")
                                    })?,
                                    matches!(request.command, Command::Next),
                                ),
                                Command::Submit {
                                    exchange_id,
                                    input_sha256,
                                    output,
                                } => {
                                    let state = state.lock().map_err(|_| {
                                        anyhow::anyhow!("session state unavailable")
                                    })?;
                                    let accepting = accepts_responses(&state);
                                    producer.submit_session_response(
                                        &identity.search_id,
                                        &exchange_id,
                                        &input_sha256,
                                        &output,
                                        ResponseWindow {
                                            deadline_ms,
                                            now_ms: now,
                                            accepting,
                                        },
                                    )
                                }
                                Command::Cancel => {
                                    producer.request_session_cancellation(&identity.search_id)?;
                                    *state.lock().map_err(|_| {
                                        anyhow::anyhow!("session state unavailable")
                                    })? = json!({"status":"cancellation_pending"});
                                    let status = cancel()?;
                                    *state.lock().map_err(|_| {
                                        anyhow::anyhow!("session state unavailable")
                                    })? = public_progress(&status);
                                    producer.session_view(
                                        &identity.search_id,
                                        deadline_ms,
                                        now,
                                        &*state.lock().map_err(|_| {
                                            anyhow::anyhow!("session state unavailable")
                                        })?,
                                        false,
                                    )
                                }
                            }
                        })();
                        let value = match answer {
                            Ok(view) => json!({"ok":true,"view":view}),
                            Err(_) => {
                                json!({"ok":false,"error":"session_request_rejected","next_operation":"status"})
                            }
                        };
                        if let Ok(bytes) = serde_json::to_vec(&value) {
                            let _ = stream.write_all(&bytes);
                            let _ = stream.write_all(b"\n");
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    Err(_) => break,
                }
            }
        });
        let bridge = Self {
            connection,
            connection_file: connection_file.into(),
            stop,
            progress,
            producer,
            deadline_ms,
            thread: Some(thread),
        };
        replace_public_file(connection_file, &serde_jcs::to_vec(&bridge.connection)?)?;
        Ok(bridge)
    }
    pub fn update(&self, status: &Value) -> Result<()> {
        let public = public_progress(status);
        *self
            .progress
            .lock()
            .map_err(|_| anyhow::anyhow!("session state unavailable"))? = public.clone();
        self.persist_view()
    }
    fn persist_view(&self) -> Result<()> {
        let progress = self
            .progress
            .lock()
            .map_err(|_| anyhow::anyhow!("session state unavailable"))?;
        let mut view = self.producer.session_view(
            &self.connection.search_id,
            self.deadline_ms,
            now_ms()?,
            &progress,
            false,
        )?;
        view["connected"] = json!(false);
        view["observed_at_ms"] = json!(now_ms()?);
        replace_public_file(
            &self.connection_file.with_extension("status.json"),
            &serde_jcs::to_vec(&view)?,
        )
    }
    /// The requester has validated the submitted K/D/receipt. Publish only its
    /// identity and digest; captured HTTP/owner values stay in private evidence.
    pub fn finish(&self, result: &Value, execution_pin: Value) -> Result<()> {
        {
            let mut progress = self
                .progress
                .lock()
                .map_err(|_| anyhow::anyhow!("session state unavailable"))?;
            progress["execution_pin"] = execution_pin;
            progress["rounds_consumed"] = result["rounds_consumed"].clone();
            progress["receipt"] = if result["submission"].is_null() {
                Value::Null
            } else {
                let submitted = &result["submission"];
                json!({"contract_ref":submitted["contract_ref"],"derivation_ref":submitted["derivation_ref"],
                    "attempt_id":submitted["attempt_id"],"fully_satisfied":submitted["receipt"]["fully_satisfied"],
                    "receipt_sha256":digest(&serde_jcs::to_vec(&submitted["receipt"])?),
                    "status":submitted["status"]})
            };
            progress["ACK"] = json!("unknown; reconcile the owner and Runtime evidence");
        }
        self.persist_view()
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = self.persist_view();
    }
}
fn same_token(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}
fn now_ms() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}
fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    BufReader::new(stream)
        .take(FRAME_CAP as u64 + 1)
        .read_until(b'\n', &mut bytes)?;
    ensure!(
        bytes.len() <= FRAME_CAP && bytes.last() == Some(&b'\n'),
        "session frame bounds"
    );
    Ok(bytes)
}
fn replace_public_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    staged.persist(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}
pub fn read_connection(path: &Path) -> Result<Connection> {
    let connection: Connection = serde_json::from_slice(&bounded_read(path, 4096)?)
        .map_err(|_| anyhow::anyhow!("invalid Session connection"))?;
    validate_connection(&connection)?;
    Ok(connection)
}
fn validate_connection(connection: &Connection) -> Result<()> {
    ensure!(
        connection.schema == CONNECTION_SCHEMA
            && connection.address.ip().is_loopback()
            && connection.address.port() != 0
            && connection.access_token.len() == 44,
        "invalid Session connection"
    );
    Ok(())
}
/// Token-free CLI configuration: only this scoped capability reaches the agent.
pub fn request(connection_file: &Path, command: Command) -> Result<Value> {
    let connection = read_connection(connection_file)?;
    request_bound(
        &connection,
        &connection_file.with_extension("status.json"),
        command,
    )
}
/// Fixed-scope tool facades reuse this transport without rereading or replacing
/// their accepted Search/configuration/capability binding.
pub fn request_bound(
    connection: &Connection,
    status_file: &Path,
    command: Command,
) -> Result<Value> {
    validate_connection(connection)?;
    let mut stream = match TcpStream::connect_timeout(&connection.address, Duration::from_secs(2)) {
        Ok(stream) => stream,
        Err(error) if matches!(command, Command::Status | Command::Next) => {
            if !status_file.exists() {
                return Err(error.into());
            }
            let mut saved: Value = serde_json::from_slice(&bounded_read(status_file, FRAME_CAP)?)?;
            ensure!(
                saved["schema"] == VIEW_SCHEMA
                    && saved["search_id"] == connection.search_id
                    && saved["configuration_ref"] == connection.configuration_ref,
                "session status binding changed"
            );
            saved["next_operation"] = json!("owner_reconnect_or_assess");
            // A disconnected snapshot is evidence only: no new input or retry authority.
            return Ok(saved);
        }
        Err(error) => return Err(error.into()),
    };
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    // Preserve RawValue bytes (including duplicate fields for strict rejection).
    // JCS's Value conversion would erase this boundary before validation.
    let bytes = serde_json::to_vec(&Request {
        access_token: connection.access_token.clone(),
        search_id: connection.search_id.clone(),
        configuration_ref: connection.configuration_ref.clone(),
        command,
    })?;
    ensure!(bytes.len() < FRAME_CAP, "session request bounds");
    stream.write_all(&bytes)?;
    stream.write_all(b"\n")?;
    let reply: Value = serde_json::from_slice(&read_frame(&mut stream)?)?;
    ensure!(
        reply["ok"] == true,
        "Session request rejected; inspect status before continuing"
    );
    Ok(reply["view"].clone())
}
fn accepts_responses(progress: &Value) -> bool {
    matches!(progress["status"].as_str(), Some("pending" | "running"))
        && progress["pause_reason"] != "needs_input"
        && progress["unresolved_attempts"].as_u64().unwrap_or(0) == 0
}
/// Deliberate whitelist: no failure bodies, variable values, ticket, grant,
/// raw response, captured observation or private cleanup errors cross the bridge.
pub fn public_progress(status: &Value) -> Value {
    let attempts = status["attempts"].as_array().cloned().unwrap_or_default();
    let public_attempts: Vec<_> = attempts.iter().map(|a| json!({
        "attempt_id":a["attempt_id"],"derivation_ref":a["derivation_ref"],"status":a["status"],
        "finished_at":a["finished_at"],"cleanup_state":a["formation_attempt"]["outcomes"]["cleanup"]["state"],
        "execution_started":a["attestation"]["execution_started"],
        "candidate_stop":a["attestation"]["candidate_stop"]
    })).collect();
    let authority_attempts = status["search_state"]["attempts"].as_array();
    let unresolved = attempts
        .iter()
        .filter(|a| match a["status"].as_str() {
            Some("pending" | "claimed") => true,
            Some("unknown") => !authority_attempts.is_some_and(|items| {
                items.iter().any(|known| {
                    known["attempt_id"] == a["attempt_id"] && known["unknown_resolved"] == true
                })
            }),
            _ => false,
        })
        .count();
    let mut budget = json!({});
    for group in [
        "attempts",
        "transfer_bytes",
        "expanded_bytes",
        "stored_bytes",
    ] {
        for metric in ["max", "used", "reserved", "remaining"] {
            if let Some(n) = status["search_budget"][group][metric].as_u64() {
                budget[group][metric] = json!(n);
            }
        }
    }
    let rounds = status["search_state"].as_object().map(|_| {
        status["search_state"]["proposal_history"]
            .as_array()
            .map_or(0, Vec::len)
            + usize::from(status["search_state"]["proposal_round"].is_object())
    });
    let input_requirements: Vec<_> = status["input_requirements"]
        .as_array()
        .into_iter()
        .flatten()
        .take(32)
        .filter_map(|item| {
            let requirement: ato_formation::variables::VariableRequirement =
                serde_json::from_value(item["requirement"].clone()).ok()?;
            ato_formation::variables::validate(std::slice::from_ref(&requirement)).ok()?;
            // Account/endpoint, candidate grants and obtain/error text can carry
            // owner data. Publish only the declared slot and typed constraints.
            Some(json!({"name":requirement.name,"kind":requirement.kind,
                "resource":requirement.resource,"operation":requirement.operation,
                "phase":requirement.phase,"secret":requirement.secret,
                "artifact_embedding":requirement.artifact_embedding,
                "temporary":requirement.temporary}))
        })
        .collect();
    json!({"status":status["status"],"pause_reason":status["pause_reason"],
        "termination_reason":status["termination_reason"],"contract_ref":status["contract_ref"],
        "rounds_consumed":rounds,
        "search_budget":budget,"attempts":public_attempts,"unresolved_attempts":unresolved,
        "input_requirements":input_requirements,
        "input_cleanup":status["input_cleanup"],"execution_stop_confirmed":status["execution_stop_confirmed"],
        "approval":"not_assessed","deployed":false,"cleanup":"not_confirmed"})
}

impl ReasoningProducer {
    fn request_session_cancellation(&self, search_id: &str) -> Result<()> {
        let path = self.directory.join("cancellation.requested.json");
        let binding = serde_jcs::to_vec(
            &json!({"search_id":search_id,"configuration_ref":self.configuration_ref}),
        )?;
        if path.exists() {
            ensure!(
                bounded_read(&path, 4096)? == binding,
                "cancellation binding changed"
            );
        } else {
            save(&path, &binding)?;
        }
        self.session_cancelled.store(true, Ordering::Relaxed);
        Ok(())
    }
    fn exchange_deadline(
        &self,
        name: &str,
        bytes: &[u8],
        input: &ReasoningInput,
    ) -> Result<Option<u64>> {
        let path = self.directory.join(format!("{name}.window.json"));
        if !path.exists() {
            return Ok(None);
        }
        let window: Value = serde_json::from_slice(&bounded_read(&path, 4096)?)?;
        ensure!(
            window["input_sha256"] == digest(bytes) && window["exchange_id"] == input.call_id,
            "Session window binding changed"
        );
        Ok(Some(
            window["deadline_ms"]
                .as_u64()
                .context("Session deadline missing")?,
        ))
    }
    fn session_inputs(&self, search_id: &str) -> Result<Vec<(String, Vec<u8>, ReasoningInput)>> {
        let mut paths = std::fs::read_dir(&self.directory)?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        let mut inputs = Vec::new();
        for path in paths {
            let Some(name) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".input.json"))
            else {
                continue;
            };
            let bytes = bounded_read(&path, 64 * 1024)?;
            let input: ReasoningInput = serde_json::from_slice(&bytes)?;
            ensure!(
                input.schema == INPUT_SCHEMA && input.request.search_id == search_id,
                "session input Search mismatch"
            );
            inputs.push((name.into(), bytes, input));
        }
        Ok(inputs)
    }
    pub fn session_view(
        &self,
        search_id: &str,
        deadline_ms: u64,
        now: u64,
        progress: &Value,
        include_input: bool,
    ) -> Result<Value> {
        let inputs = self.session_inputs(search_id)?;
        let start_path = self.directory.with_extension("start.json");
        let elapsed = if start_path.exists() {
            let start: Value = serde_json::from_slice(&bounded_read(&start_path, 4096)?)?;
            if start["search_id"] == search_id
                && start["configuration_ref"] == self.configuration_ref
                && start["deadline_ms"] == deadline_ms
            {
                start["started_at_ms"]
                    .as_u64()
                    .map(|started| now.saturating_sub(started))
            } else {
                None // Older checkpoints have no trustworthy start-time binding.
            }
        } else {
            None
        };
        let records = self.records()?;
        let completed_inspections = records
            .iter()
            .filter(|(_, _, r)| !r.inspected.is_empty() || r.inspection_error.is_some())
            .count();
        let inspection_ms = records.iter().fold(0_u64, |total, (_, _, r)| {
            total.saturating_add(r.inspection_ms)
        });
        let pending: Vec<_> = inputs
            .iter()
            .filter(|(name, _, _)| !self.directory.join(format!("{name}.record.json")).exists())
            .collect();
        let mut view = json!({"schema":VIEW_SCHEMA,"search_id":search_id,
            "configuration_ref":self.configuration_ref,"deadline_ms":deadline_ms,
            "search_elapsed_ms":elapsed,
            "exchanges_used":inputs.len(),"exchanges_remaining":self.budget.max_calls.saturating_sub(inputs.len() as u32),
            "inspection_exchanges_completed":completed_inspections,"inspection_elapsed_ms":inspection_ms,
            "source_identity":inputs.first().map(|(_, _, i)| &i.source_identity),
            "internal_LLM_calls":"unknown","token_usage":"unknown","cost":"unknown",
            "progress":progress,"connected":true,"input":null,"exchange":null});
        if pending.len() > 1 {
            view["waiting_reason"] = json!("unresolved_exchanges");
            view["next_operation"] = json!("owner_reconcile");
        } else if now >= deadline_ms
            || !accepts_responses(progress)
            || self.session_cancelled.load(Ordering::Relaxed)
        {
            view["waiting_reason"] = json!(if now >= deadline_ms {
                "deadline_exceeded"
            } else {
                "coordinator_wait_or_terminal"
            });
            view["next_operation"] = json!("owner_reconcile_or_assess");
        } else if let Some((name, bytes, input)) = pending.first() {
            let Some(exchange_deadline) = self.exchange_deadline(name, bytes, input)? else {
                view["waiting_reason"] = json!("exchange_window_unavailable");
                view["next_operation"] = json!("owner_reconcile");
                return Ok(view);
            };
            view["exchange_deadline_ms"] = json!(exchange_deadline.min(deadline_ms));
            if now >= exchange_deadline {
                view["waiting_reason"] = json!("exchange_deadline_exceeded");
                view["next_operation"] = json!("owner_reconcile");
                return Ok(view);
            }
            let response_saved = self
                .directory
                .join(format!("{name}.response.json"))
                .exists();
            view["exchange"] = json!({"exchange_id":input.call_id,"input_sha256":digest(bytes),"response_saved":response_saved});
            view["waiting_reason"] = json!(if response_saved {
                "saved_response_awaiting_reconciliation"
            } else {
                "awaiting_producer"
            });
            view["next_operation"] = json!(if response_saved { "status" } else { "submit" });
            if include_input && !response_saved {
                view["input"] = serde_json::to_value(input)?;
                let ReasoningProviderConfig::Session(config) = &self.config else {
                    bail!("Session configuration missing")
                };
                view["instructions"] = json!(
                    deepseek::prompt_for(&config.prompt_version)
                        .context("Session prompt missing")?
                );
            }
        } else {
            view["waiting_reason"] = json!("coordinator_or_runtime");
            view["next_operation"] = json!("status");
        }
        Ok(view)
    }
    pub fn submit_session_response(
        &self,
        search_id: &str,
        exchange_id: &str,
        input_sha256: &str,
        output: &serde_json::value::RawValue,
        window: ResponseWindow,
    ) -> Result<Value> {
        let ResponseWindow {
            deadline_ms,
            now_ms: now,
            accepting,
        } = window;
        ensure!(self.config.is_session(), "Session transport required");
        let inputs = self.session_inputs(search_id)?;
        let (name, bytes, input) = inputs
            .iter()
            .find(|(_, _, i)| i.call_id == exchange_id)
            .context("unknown Session exchange")?;
        ensure!(
            digest(bytes) == input_sha256,
            "Session input digest mismatch"
        );
        let raw = output.get().as_bytes();
        ensure!(
            raw.len() <= ato_formation::proposal::MAX_BATCH_BYTES,
            "Session output bounds"
        );
        let batch: ProposalBatch = serde_json::from_slice(raw)?;
        ensure!(
            batch.schema == ato_formation::proposal::PROPOSAL_SCHEMA && batch.proposals.len() == 1,
            "one typed inspection, proposal or decline required"
        );
        let response = json!({"schema":SESSION_RESPONSE_SCHEMA,"input_sha256":input_sha256,
            "exchange_id":exchange_id,"output":batch});
        let response_path = self.directory.join(format!("{name}.response.json"));
        let saved_bytes = serde_jcs::to_vec(&response)?;
        if response_path.exists() {
            let saved: SessionResponse =
                serde_json::from_slice(&bounded_read(&response_path, 20 * 1024)?)?;
            ensure!(
                saved.schema == SESSION_RESPONSE_SCHEMA
                    && saved.input_sha256 == input_sha256
                    && saved
                        .exchange_id
                        .as_ref()
                        .is_none_or(|id| id == exchange_id)
                    && serde_json::from_str::<ProposalBatch>(saved.output.get())? == batch,
                "Session response conflict"
            );
        } else {
            let exchange_deadline = self
                .exchange_deadline(name, bytes, input)?
                .context("Session window unavailable")?;
            ensure!(
                accepting
                    && now < deadline_ms
                    && now < exchange_deadline
                    && !self.session_cancelled.load(Ordering::Relaxed),
                "Session response window closed"
            );
            ensure!(
                inputs
                    .last()
                    .is_some_and(|(_, _, i)| i.call_id == exchange_id)
                    && !self.directory.join(format!("{name}.record.json")).exists(),
                "stale Session exchange"
            );
            match save(&response_path, &saved_bytes) {
                Ok(()) => (),
                Err(error) if response_path.exists() => {
                    ensure!(
                        bounded_read(&response_path, 20 * 1024)? == saved_bytes,
                        "Session response conflict"
                    );
                    drop(error);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(
            json!({"schema":VIEW_SCHEMA,"search_id":search_id,"exchange_id":exchange_id,
            "input_sha256":input_sha256,"response_saved":true,"next_operation":"status",
            "approval":"not_assessed","execution_permission":false}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plan() -> budget::BudgetPlan {
        budget::BudgetPlan {
            max_calls: 6,
            input_token_cap: 49152,
            output_token_cap: 2048,
            input_price: 300000,
            output_price: 1200000,
            ceiling_usd_micros: 103224,
        }
    }
    fn config(agent: Option<SessionAgent>) -> ReasoningProviderConfig {
        ReasoningProviderConfig::Session(SessionConfig {
            provider: if agent.is_some() {
                "agent_session"
            } else {
                "codex_session"
            }
            .into(),
            model: agent
                .as_ref()
                .and_then(|a| a.model.clone())
                .unwrap_or_else(|| {
                    if agent.is_some() {
                        "unknown"
                    } else {
                        "codex-session"
                    }
                    .into()
                }),
            prompt_version: deepseek::PROMPT_VERSION_V13.into(),
            agent,
        })
    }
    fn fixture() -> Result<(tempfile::TempDir, Arc<ReasoningProducer>, String)> {
        std::fs::create_dir_all(".tmp")?;
        let root = tempfile::tempdir_in(".tmp")?;
        let producer = Arc::new(ReasoningProducer::new(
            config(None),
            plan(),
            root.path().join("reasoning"),
            None,
        )?);
        let input = json!({"schema":INPUT_SCHEMA,"call_id":"search_test_r1_s1","goal":null,
            "frozen_contract_ref":format!("sha256:{}","a".repeat(64)),
            "source_identity":{"archive_digest":format!("sha256:{}","b".repeat(64)),"closure_ref":"closure-test"},
            "inventory":[],"projection":[],"rounds_remaining":3,"calls_remaining":6,
            "inspections_remaining":4,"inspection_source_bytes_remaining":65536,"inspection_feedback":[],
            "request":{"schema":"ato.formation-proposal-request/2","search_id":"search_test","round_seq":1,
                "frozen_contract":{"schema":"ato.contract/1","requirements":[{"id":"root","verifier":"ato.contract.http@1","port":"app.http","method":"GET","path":"/","status":200}]},
                "runtime_constraint":{"kind":"any"},"known_derivations":[],"failure_evidence":[],"inspection_evidence":[],
                "operation_catalog":{"schema":"ato.formation-operation-catalog/1","operations":[]},
                "remaining_budget":{"rounds_remaining":3,"max_proposals":1,"timeout_ms":30000,"attempts_remaining":3},"source_context":[]}});
        let bytes = serde_jcs::to_vec(&input)?;
        let _: ReasoningInput = serde_json::from_slice(&bytes)?;
        save(&producer.directory.join("r001_s001.input.json"), &bytes)?;
        save(
            &producer.directory.join("r001_s001.window.json"),
            &serde_jcs::to_vec(
                &json!({"exchange_id":"search_test_r1_s1","input_sha256":digest(&bytes),"deadline_ms":1000}),
            )?,
        )?;
        Ok((root, producer, digest(&bytes)))
    }
    fn output() -> Box<serde_json::value::RawValue> {
        serde_json::value::to_raw_value(&json!({"schema":"ato.formation-proposal/1","proposals":[{"kind":"unsupported","reason":"insufficient_source"}]})).unwrap()
    }
    #[test]
    fn elapsed_time_uses_only_the_original_bound_start_checkpoint() -> Result<()> {
        let (_root, producer, _) = fixture()?;
        let progress = public_progress(&json!({"status":"running","attempts":[]}));
        let path = producer.directory.with_extension("start.json");
        save(&path, &serde_jcs::to_vec(&json!({"deadline_ms":1000}))?)?;
        assert!(
            producer.session_view("search_test", 1000, 100, &progress, false)?["search_elapsed_ms"]
                .is_null()
        );
        replace_public_file(
            &path,
            &serde_jcs::to_vec(&json!({
                "search_id":"search_test","configuration_ref":producer.configuration_ref,
                "deadline_ms":1000,"started_at_ms":10
            }))?,
        )?;
        assert_eq!(
            producer.session_view("search_test", 1000, 100, &progress, false)?["search_elapsed_ms"],
            90
        );
        assert_eq!(
            producer.session_view("search_test", 1000, 200, &progress, false)?["search_elapsed_ms"],
            190
        );
        assert!(
            producer.session_view("search_test", 2000, 200, &progress, false)?["search_elapsed_ms"]
                .is_null()
        );
        Ok(())
    }

    #[test]
    fn legacy_digest_and_both_agent_bindings_are_preserved() -> Result<()> {
        let legacy = config(None);
        let c = json!({"provider":"codex_session","model":"codex-session","prompt_version":deepseek::PROMPT_VERSION_V13});
        let expected = digest(&serde_jcs::to_vec(&(
            c,
            plan(),
            deepseek::prompt_for(deepseek::PROMPT_VERSION_V13).unwrap(),
        ))?);
        assert_eq!(legacy.configuration_ref(&plan())?, expected);
        for kind in [SessionAgentKind::Codex, SessionAgentKind::ClaudeCode] {
            let first = config(Some(SessionAgent {
                kind: kind.clone(),
                version: "test-1".into(),
                model: None,
            }));
            let second = config(Some(SessionAgent {
                kind,
                version: "test-2".into(),
                model: None,
            }));
            assert_ne!(
                first.configuration_ref(&plan())?,
                second.configuration_ref(&plan())?
            );
        }
        let bad = ReasoningProviderConfig::Session(SessionConfig {
            provider: "agent_session".into(),
            model: "inferred".into(),
            prompt_version: deepseek::PROMPT_VERSION_V13.into(),
            agent: Some(SessionAgent {
                kind: SessionAgentKind::ClaudeCode,
                version: "test".into(),
                model: None,
            }),
        });
        assert!(bad.configuration_ref(&plan()).is_err());
        Ok(())
    }
    #[test]
    fn repeated_reads_and_responses_survive_disconnect_without_replenishment() -> Result<()> {
        let (root, p, digest) = fixture()?;
        let progress = json!({"status":"pending","unresolved_attempts":0});
        let first = p.session_view("search_test", 1000, 10, &progress, true)?;
        assert_eq!(
            first,
            p.session_view("search_test", 1000, 10, &progress, true)?
        );
        assert_eq!(first["exchanges_used"], 1);
        p.submit_session_response(
            "search_test",
            "search_test_r1_s1",
            &digest,
            &output(),
            ResponseWindow {
                deadline_ms: 1000,
                now_ms: 10,
                accepting: true,
            },
        )?;
        let restored =
            ReasoningProducer::new(config(None), plan(), root.path().join("reasoning"), None)?;
        let waiting = restored.session_view("search_test", 1000, 11, &progress, true)?;
        assert!(waiting["input"].is_null());
        assert_eq!(waiting["exchange"]["response_saved"], true);
        assert_eq!(waiting["exchanges_remaining"], 5);
        // Identical bytes can be reconciled after deadline; no new response is created.
        restored.submit_session_response(
            "search_test",
            "search_test_r1_s1",
            &digest,
            &output(),
            ResponseWindow {
                deadline_ms: 1000,
                now_ms: 2000,
                accepting: false,
            },
        )?;
        let changed = serde_json::value::to_raw_value(
            &json!({"schema":"ato.formation-proposal/1","proposals":[{"kind":"unsupported","reason":"no_progress"}]}),
        )?;
        assert!(
            restored
                .submit_session_response(
                    "search_test",
                    "search_test_r1_s1",
                    &digest,
                    &changed,
                    ResponseWindow {
                        deadline_ms: 1000,
                        now_ms: 12,
                        accepting: true
                    }
                )
                .is_err()
        );
        Ok(())
    }
    #[test]
    fn stale_digest_and_blocked_windows_cannot_publish() -> Result<()> {
        let (_root, p, digest) = fixture()?;
        assert!(
            p.submit_session_response(
                "search_test",
                "search_test_r1_s1",
                "wrong",
                &output(),
                ResponseWindow {
                    deadline_ms: 1000,
                    now_ms: 10,
                    accepting: true
                }
            )
            .is_err()
        );
        assert!(
            p.submit_session_response(
                "search_test",
                "search_test_r1_s1",
                &digest,
                &output(),
                ResponseWindow {
                    deadline_ms: 1000,
                    now_ms: 1000,
                    accepting: true
                }
            )
            .is_err()
        );
        assert!(
            p.submit_session_response(
                "search_test",
                "search_test_r1_s1",
                &digest,
                &output(),
                ResponseWindow {
                    deadline_ms: 1000,
                    now_ms: 10,
                    accepting: false
                }
            )
            .is_err()
        );
        let old = std::fs::read(p.directory.join("r001_s001.input.json"))?;
        let mut newer: Value = serde_json::from_slice(&old)?;
        newer["call_id"] = json!("search_test_r2_s1");
        newer["request"]["round_seq"] = json!(2);
        save(
            &p.directory.join("r002_s001.input.json"),
            &serde_jcs::to_vec(&newer)?,
        )?;
        assert!(
            p.submit_session_response(
                "search_test",
                "search_test_r1_s1",
                &digest,
                &output(),
                ResponseWindow {
                    deadline_ms: 1000,
                    now_ms: 10,
                    accepting: true
                }
            )
            .is_err()
        );
        assert_eq!(
            p.session_view("search_test", 1000, 10, &json!({"status":"pending"}), true)?["waiting_reason"],
            "unresolved_exchanges"
        );
        Ok(())
    }
    #[test]
    fn concurrent_submissions_and_duplicate_owner_are_fenced() -> Result<()> {
        let (root, p, digest) = fixture()?;
        let journal = root.path().join("producer.jsonl");
        let guard = lock_owner_journal(&journal)?;
        assert!(lock_owner_journal(&journal).is_err());
        drop(guard);
        let _new_owner = lock_owner_journal(&journal)?;
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let (p, digest) = (p.clone(), digest.clone());
                std::thread::spawn(move || {
                    p.submit_session_response(
                        "search_test",
                        "search_test_r1_s1",
                        &digest,
                        &output(),
                        ResponseWindow {
                            deadline_ms: 1000,
                            now_ms: 10,
                            accepting: true,
                        },
                    )
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap()?;
        }
        assert_eq!(
            std::fs::read_dir(&p.directory)?
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().ends_with("response.json"))
                .count(),
            1
        );
        Ok(())
    }
    #[test]
    fn bridge_authentication_binding_and_private_projection_are_enforced() -> Result<()> {
        let (root, p, digest) = fixture()?;
        let path = root.path().join("agent/connection.json");
        let deadline = now_ms()? + 60000;
        replace_public_file(
            &p.directory.join("r001_s001.window.json"),
            &serde_jcs::to_vec(
                &json!({"exchange_id":"search_test_r1_s1","input_sha256":digest,"deadline_ms":deadline}),
            )?,
        )?;
        let bridge = Bridge::start(
            p,
            "search_test",
            deadline,
            &path,
            [7; 32],
            Arc::new(|| Ok(json!({"status":"stopped","attempts":[]}))),
        )?;
        let private = "OWNER_VALUE_SENTINEL";
        bridge.update(&json!({"status":"pending","pause_reason":null,"token":private,"grant":private,
            "attempts":[{"attempt_id":"attempt-test","status":"finished","ticket":private,"failure":{"message":private},"formation_attempt":{"logs":private}}],
            "search_budget":{"attempts":{"max":3,"used":1,"reserved":0,"remaining":2},"private":private}}))?;
        let next = request(&path, Command::Next)?;
        assert!(next["input"].is_object());
        assert!(!serde_json::to_string(&next)?.contains(private));
        let mut malformed = serde_json::to_value(read_connection(&path)?)?;
        malformed["agent"] = json!({"kind":private,"version":"fixture"});
        let malformed_path = root.path().join("malformed.json");
        replace_public_file(&malformed_path, &serde_jcs::to_vec(&malformed)?)?;
        assert!(
            !format!("{:#}", read_connection(&malformed_path).err().unwrap()).contains(private)
        );
        let mut wrong = read_connection(&path)?;
        wrong.access_token = BASE64.encode([8; 32]);
        let forged = root.path().join("forged.json");
        replace_public_file(&forged, &serde_jcs::to_vec(&wrong)?)?;
        assert!(request(&forged, Command::Status).is_err());
        request(
            &path,
            Command::Submit {
                exchange_id: "search_test_r1_s1".into(),
                input_sha256: digest,
                output: output(),
            },
        )?;
        assert!(request(&path, Command::Next)?["input"].is_null());
        request(&path, Command::Cancel)?;
        assert_eq!(
            request(&path, Command::Status)?["progress"]["status"],
            "stopped"
        );
        drop(bridge);
        let archived = request(&path, Command::Status)?;
        assert_eq!(archived["connected"], false);
        assert!(archived["input"].is_null());
        Ok(())
    }
    #[test]
    fn input_wait_projects_only_validated_slot_metadata() -> Result<()> {
        let (_root, producer, _) = fixture()?;
        let private = "OWNER_VALUE_SENTINEL";
        let status = json!({"status":"running","pause_reason":"needs_input","attempts":[],
            "input_requirements":[{"requirement":{"name":"OWNER_EMAIL","kind":"configuration",
                "purpose":private,"account":private,"endpoint":private,"tenant":private,
                "service":private,"resource":"account","operation":"read","phase":"runtime",
                "secret":false},"reason":private,"obtain":private,"candidate_grants":[private]},
                {"requirement":{"name":private,"value":private}}]});
        let progress = public_progress(&status);
        assert_eq!(progress["input_requirements"].as_array().unwrap().len(), 1);
        assert_eq!(progress["input_requirements"][0]["name"], "OWNER_EMAIL");
        assert_eq!(progress["input_requirements"][0]["secret"], false);
        assert!(!serde_json::to_string(&progress)?.contains(private));
        assert!(!accepts_responses(&progress));
        let view = producer.session_view("search_test", 1000, 10, &progress, true)?;
        assert!(view["input"].is_null());
        assert_eq!(view["exchanges_used"], 1);
        assert_eq!(view["exchanges_remaining"], 5);
        Ok(())
    }
    #[test]
    fn unknown_connecting_and_duplicate_fields_fail_closed() -> Result<()> {
        let (_root, p, digest) = fixture()?;
        for state in ["unknown", "connecting", "satisfied", "stopped"] {
            let progress = public_progress(&json!({"status":state,"attempts":[]}));
            assert!(!accepts_responses(&progress));
            assert!(p.session_view("search_test", 1000, 10, &progress, true)?["input"].is_null());
        }
        let duplicate: Box<serde_json::value::RawValue> = serde_json::from_str(
            r#"{"schema":"ato.formation-proposal/1","proposals":[{"kind":"unsupported","reason":"no_progress","reason":"insufficient_source"}]}"#,
        )?;
        assert!(
            p.submit_session_response(
                "search_test",
                "search_test_r1_s1",
                &digest,
                &duplicate,
                ResponseWindow {
                    deadline_ms: 1000,
                    now_ms: 10,
                    accepting: true
                }
            )
            .is_err()
        );
        assert!(!p.directory.join("r001_s001.response.json").exists());
        let resolved = public_progress(
            &json!({"status":"running","attempts":[{"attempt_id":"a","status":"unknown"}],
            "search_state":{"attempts":[{"attempt_id":"a","unknown_resolved":true}]}}),
        );
        assert_eq!(resolved["unresolved_attempts"], 0);
        assert!(accepts_responses(&resolved));
        Ok(())
    }

    #[test]
    fn round_window_does_not_expand_to_search_deadline() -> Result<()> {
        let (_root, p, digest) = fixture()?;
        let progress = json!({"status":"pending"});
        let view = p.session_view("search_test", 2000, 1000, &progress, true)?;
        assert!(view["input"].is_null());
        assert_eq!(view["waiting_reason"], "exchange_deadline_exceeded");
        assert!(
            p.submit_session_response(
                "search_test",
                "search_test_r1_s1",
                &digest,
                &output(),
                ResponseWindow {
                    deadline_ms: 2000,
                    now_ms: 1000,
                    accepting: true
                }
            )
            .is_err()
        );
        Ok(())
    }
    #[test]
    fn lost_cancel_ack_blocks_reproposal_and_survives_owner_restart() -> Result<()> {
        let (root, p, digest) = fixture()?;
        let path = root.path().join("agent/connection.json");
        let deadline = now_ms()? + 60000;
        replace_public_file(
            &p.directory.join("r001_s001.window.json"),
            &serde_jcs::to_vec(
                &json!({"exchange_id":"search_test_r1_s1","input_sha256":digest,"deadline_ms":deadline}),
            )?,
        )?;
        let bridge = Bridge::start(
            p.clone(),
            "search_test",
            deadline,
            &path,
            [9; 32],
            Arc::new(|| bail!("lost cancel ACK")),
        )?;
        bridge.update(&json!({"status":"running","attempts":[]}))?;
        assert!(request(&path, Command::Cancel).is_err());
        assert!(request(&path, Command::Next)?["input"].is_null());
        assert!(
            p.submit_session_response(
                "search_test",
                "search_test_r1_s1",
                &digest,
                &output(),
                ResponseWindow {
                    deadline_ms: deadline,
                    now_ms: now_ms()?,
                    accepting: true
                }
            )
            .is_err()
        );
        drop(bridge);
        let resumed =
            ReasoningProducer::new(config(None), plan(), root.path().join("reasoning"), None)?;
        assert!(
            resumed.session_view(
                "search_test",
                deadline,
                now_ms()?,
                &json!({"status":"running"}),
                true
            )?["input"]
                .is_null()
        );
        Ok(())
    }
}
