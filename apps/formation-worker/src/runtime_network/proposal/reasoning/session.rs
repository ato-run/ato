//! A scoped CandidateProducer bridge. It exposes saved public input, never the
//! owner journal, API token, source workspace, Runtime ticket or receipt body.
use super::*;
use std::{
    io::{BufRead, BufReader},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
};

pub const CONNECTION_SCHEMA: &str = "ato.formation-session-connection/2";
pub const VIEW_SCHEMA: &str = "ato.formation-session-view/1";
pub const INPUT_PROJECTION_SCHEMA: &str = "ato.formation-session-public-input/1";
const FRAME_CAP: usize = 96 * 1024;

// Owner metadata is valid for the owner API, but its free text is not public
// CandidateProducer input. Parse the complete envelope before projecting slots.
#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum VariableReuse {
    ThisFormation,
    Reusable,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerVariableMetadata {
    requirement: ato_formation::variables::VariableRequirement,
    applications: Vec<String>,
    reuse: VariableReuse,
    formation_id: Option<String>,
    expires_at_ms: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerVariable {
    metadata: OwnerVariableMetadata,
    revoked_at_ms: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerRuntimeEnvironment {
    environment_id: String,
    facts_ref: String,
    facts: BTreeMap<String, String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerRuntimeAvailability {
    online: bool,
    observed_at: Option<String>,
    capacity: Option<u16>,
    current_slots: Option<u16>,
    health: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerRuntime {
    runtime_id: String,
    environments: Vec<OwnerRuntimeEnvironment>,
    availability: OwnerRuntimeAvailability,
}

fn public_variable_requirement(r: &ato_formation::variables::VariableRequirement) -> Value {
    json!({"name":r.name,"kind":r.kind,"resource":r.resource,"operation":r.operation,
        "phase":r.phase,"secret":r.secret,"artifact_embedding":r.artifact_embedding,
        "temporary":r.temporary})
}
fn canonical_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn public_variable(value: &Value, input: &ReasoningInput) -> Option<Value> {
    let owner: OwnerVariable = serde_json::from_value(value.clone()).ok()?;
    let m = owner.metadata;
    ato_formation::variables::validate(std::slice::from_ref(&m.requirement)).ok()?;
    const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
    if !(1..=MAX_SAFE_INTEGER).contains(&m.expires_at_ms)
        || owner.revoked_at_ms.is_some_and(|n| n > MAX_SAFE_INTEGER)
        || !(1..=32).contains(&m.applications.len())
        || !m.applications.iter().all(|a| canonical_sha256(a))
        || !m.applications.contains(&input.source_identity.closure_ref)
        || match m.reuse {
            VariableReuse::ThisFormation => {
                m.formation_id.as_deref() != Some(&input.request.search_id)
            }
            VariableReuse::Reusable => m.formation_id.is_some() || m.requirement.temporary,
        }
    {
        return None;
    }
    Some(
        json!({"metadata":{"requirement":public_variable_requirement(&m.requirement),
        "reuse":m.reuse,"expires_at_ms":m.expires_at_ms},"revoked_at_ms":owner.revoked_at_ms}),
    )
}

/// Exact fact names come from the same typed authorization as the compiler;
/// arbitrary toolchain/source-OCI prefixes are never an information boundary.
fn catalog_fact_allowlist(input: &ReasoningInput) -> BTreeMap<String, &'static str> {
    let mut facts = BTreeMap::new();
    if input.request.operation_catalog.schema != "ato.formation-operation-catalog/1" {
        return facts;
    }
    for domain in &input.request.operation_catalog.operations {
        let ato_formation::proposal::OperationDomain::ExecutionPlan {
            runtime_port_operations,
            toolchains,
            source_oci,
            ..
        } = domain
        else {
            continue;
        };
        let auth = ato_formation::proposal::PlanAuthorization {
            runtime_port_operations: *runtime_port_operations,
            files: BTreeMap::new(),
            toolchains: toolchains.clone(),
            source_oci: source_oci.clone(),
        };
        if auth.validate().is_err() {
            continue;
        }
        for (name, version) in toolchains {
            facts.insert(format!("toolchain.{name}.{version}"), "present");
        }
        if let Some(recipe) = source_oci {
            for image in &recipe.base_images {
                facts.insert(
                    format!(
                        "formation.oci.image.{}",
                        image.pinned_digest.trim_start_matches("sha256:")
                    ),
                    "boolean",
                );
            }
        }
    }
    facts
}
fn public_fact(key: &str, value: &str, exact: &BTreeMap<String, &'static str>) -> bool {
    match key {
        "runtime.process"
        | "runtime.oci"
        | "formation.source_oci.available"
        | "formation.source_oci.bound"
        | ato_formation::port_operations::RUNTIME_CAPABILITY => matches!(value, "true" | "false"),
        "os" => matches!(value, "linux" | "macos" | "windows"),
        "arch" => matches!(value, "x86_64" | "aarch64" | "x86" | "arm" | "riscv64"),
        "containment" | "formation.containment" => matches!(value, "bwrap+landlock" | "none"),
        _ => match exact.get(key) {
            Some(&"present") => value == "present",
            Some(&"boolean") => matches!(value, "true" | "false"),
            _ => false,
        },
    }
}
fn public_runtime(value: &Value, exact: &BTreeMap<String, &'static str>) -> Option<Value> {
    let owner: OwnerRuntime = serde_json::from_value(value.clone()).ok()?;
    let a = owner.availability;
    if owner.runtime_id.is_empty()
        || owner.runtime_id.len() > 256
        || !owner
            .runtime_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        || !(1..=8).contains(&owner.environments.len())
        || a.capacity.is_some_and(|n| n > 1024)
        || a.current_slots.is_some_and(|n| n > 1024)
        || a.health
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 32)
        || a.observed_at.as_ref().is_some_and(|s| s.len() > 64)
        || owner.environments.iter().any(|e| {
            e.environment_id.is_empty()
                || e.environment_id.len() > 64
                || !e.environment_id.as_bytes()[0].is_ascii_lowercase()
                    && !e.environment_id.as_bytes()[0].is_ascii_digit()
                || !e
                    .environment_id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
                || !canonical_sha256(&e.facts_ref)
                || e.facts.len() > 128
                || e.facts.iter().any(|(key, value)| {
                    key.is_empty()
                        || key.len() > 128
                        || !key.bytes().all(|b| {
                            b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-/".contains(&b)
                        })
                        || value.len() > 256
                })
        })
    {
        return None;
    }
    let environments: Vec<_> = owner
        .environments
        .into_iter()
        .map(|e| {
            let facts: BTreeMap<_, _> = e
                .facts
                .into_iter()
                .filter(|(key, value)| public_fact(key, value, exact))
                .collect();
            json!({"environment_id":e.environment_id,"facts_ref":e.facts_ref,"facts":facts})
        })
        .collect();
    Some(
        json!({"runtime_id":owner.runtime_id,"environments":environments,
        "availability":{"online":a.online,"capacity":a.capacity,"current_slots":a.current_slots,
            "health":if a.health.as_deref() == Some("ok") {"ok"} else {"unknown"}}}),
    )
}
fn public_reasoning_input(input: &ReasoningInput) -> Result<Value> {
    // Keep verified Source and protocol evidence unchanged. This projection is
    // a view, not a new persisted input or an independently hashed exchange.
    let mut public = serde_json::to_value(input)?;
    public["available_variables"] = json!(
        input
            .available_variables
            .iter()
            .take(128)
            .filter_map(|v| public_variable(v, input))
            .collect::<Vec<_>>()
    );
    let exact = catalog_fact_allowlist(input);
    public["runtime_capabilities"] = json!(
        input
            .runtime_capabilities
            .iter()
            .take(16)
            .filter_map(|v| public_runtime(v, &exact))
            .collect::<Vec<_>>()
    );
    Ok(public)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub schema: String,
    pub address: SocketAddr,
    pub search_id: String,
    pub configuration_ref: String,
    pub agent: Option<SessionAgent>,
}

/// Private transport authorization is deliberately neither Debug nor Serialize.
/// A public descriptor alone cannot authorize a Session operation.
#[derive(Clone)]
pub struct AuthorizedConnection {
    pub connection: Connection,
    capability: [u8; 32],
}

impl std::ops::Deref for AuthorizedConnection {
    type Target = Connection;
    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

impl AuthorizedConnection {
    pub fn same_authorization(&self, other: &Self) -> bool {
        self.search_id == other.search_id
            && self.configuration_ref == other.configuration_ref
            && self.agent == other.agent
            && same_token(
                &BASE64.encode(self.capability),
                &BASE64.encode(other.capability),
            )
    }
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

/// Only the trusted requester creates/reconnects this server. The public
/// descriptor and private capability are separate; only the owner CLI cancels.
#[derive(Clone, Copy)]
pub struct ResponseWindow {
    pub deadline_ms: u64,
    pub now_ms: u64,
    pub accepting: bool,
}

pub struct Bridge {
    connection: AuthorizedConnection,
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
            search_id: search_id.into(),
            configuration_ref: producer.configuration_ref.clone(),
            agent,
        };
        let capability = if connection_file.exists() {
            let saved = read_authorized_connection(connection_file)?;
            ensure!(
                saved.search_id == connection.search_id
                    && saved.configuration_ref == connection.configuration_ref
                    && saved.agent == connection.agent,
                "session connection binding changed"
            );
            saved.capability
        } else {
            publish_capability(connection_file, &connection, entropy)?;
            entropy
        };
        let listener = TcpListener::bind(connection.address)?;
        listener.set_nonblocking(true)?;
        connection.address = listener.local_addr()?;
        let connection = AuthorizedConnection {
            connection,
            capability,
        };
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
                            // Accepted sockets may inherit nonblocking mode
                            // on BSD hosts. The bounded frame reader below
                            // must wait for fragmented authenticated requests.
                            stream.set_nonblocking(false)?;
                            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                            stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                            let raw = read_frame(&mut stream)?;
                            let request: Request = serde_json::from_slice(&raw)?;
                            // Do not echo parser errors or attacker-controlled request bytes.
                            ensure!(
                                same_token(
                                    &request.access_token,
                                    &BASE64.encode(identity.capability)
                                ) && request.search_id == identity.search_id
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
        replace_public_file(
            connection_file,
            &serde_jcs::to_vec(&bridge.connection.connection)?,
        )?;
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
            && connection.address.port() != 0,
        "invalid Session connection"
    );
    Ok(())
}
fn capability_binding(connection: &Connection) -> Result<Vec<u8>> {
    Ok(Sha256::digest(serde_jcs::to_vec(&json!([
        CONNECTION_SCHEMA,
        connection.search_id,
        connection.configuration_ref,
        connection.agent
    ]))?)
    .to_vec())
}

fn publish_capability(path: &Path, connection: &Connection, capability: [u8; 32]) -> Result<()> {
    let private = path.with_extension("capability");
    let parent = private.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(&capability_binding(connection)?)?;
    staged.write_all(&capability)?;
    staged.as_file().sync_all()?;
    staged
        .persist_noclobber(&private)
        .map_err(|_| anyhow::anyhow!("Session capability publication rejected"))?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// Owner/MCP startup only. Native model tools never receive this path or bytes;
/// the owner launcher must keep the file outside its model OS profile.
pub fn read_authorized_connection(path: &Path) -> Result<AuthorizedConnection> {
    let connection = read_connection(path)?;
    let private = path.with_extension("capability");
    let meta = std::fs::symlink_metadata(&private)?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() == 64,
        "Session private capability rejected"
    );
    let file = std::fs::File::open(&private)?;
    let actual = file.metadata()?;
    ensure!(
        actual.is_file() && actual.len() == 64,
        "Session private capability rejected"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            meta.dev() == actual.dev() && meta.ino() == actual.ino(),
            "Session private capability changed"
        );
    }
    let mut bytes = Vec::new();
    file.take(65).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() == 64 && bytes[..32] == capability_binding(&connection)?,
        "Session private capability binding rejected"
    );
    let capability = bytes[32..]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Session capability rejected"))?;
    Ok(AuthorizedConnection {
        connection,
        capability,
    })
}

/// The owner CLI loads private authorization separately and prints public views only.
pub fn request(connection_file: &Path, command: Command) -> Result<Value> {
    let connection = read_authorized_connection(connection_file)?;
    request_bound(
        &connection,
        &connection_file.with_extension("status.json"),
        command,
    )
}
/// Fixed-scope tool facades reuse this transport without rereading or replacing
/// their accepted Search/configuration/capability binding.
pub fn request_bound(
    connection: &AuthorizedConnection,
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
        access_token: BASE64.encode(connection.capability),
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
            Some(public_variable_requirement(&requirement))
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
    pub(super) fn exchange_deadline(
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
    /// Reconcile only an already-published, input-bound response after its
    /// immutable window closes. No new input, source inspection or execution.
    pub(in crate::runtime_network::proposal) fn reconcile_expired_session_response(
        &self,
        search_id: &str,
        deadline_ms: u64,
        now: u64,
    ) -> Result<bool> {
        if !self.config.is_session() {
            return Ok(false);
        }
        let _guard = self
            .session_reconciliation
            .lock()
            .map_err(|_| anyhow::anyhow!("Session reconciliation unavailable"))?;
        let inputs = self.session_inputs(search_id)?;
        // Validate any existing receipt before treating repeated reconciliation
        // as a no-op. In particular, a changed response cannot replace it.
        self.records()?;
        let pending: Vec<_> = inputs
            .iter()
            .filter(|(name, _, _)| !self.directory.join(format!("{name}.record.json")).exists())
            .collect();
        if pending.len() > 1 {
            return Ok(false); // An ambiguous journal must remain blocked.
        }
        let Some((name, bytes, input)) = pending.first() else {
            return Ok(false);
        };
        if !self
            .directory
            .join(format!("{name}.response.json"))
            .exists()
        {
            return Ok(false);
        }
        ensure!(
            inputs.last().is_some_and(|(last, _, _)| last == name),
            "stale Session response"
        );
        let exchange_deadline = self
            .exchange_deadline(name, bytes, input)?
            .context("Session window unavailable")?;
        if now < deadline_ms.min(exchange_deadline) {
            return Ok(false);
        }
        let raw = self.saved_session_response(name, bytes, input, true)?;
        let record = StepRecord {
            schema: "ato.formation-reasoning-step/1".into(),
            configuration_ref: self.configuration_ref.clone(),
            input_sha256: digest(bytes),
            output_sha256: digest(&raw),
            raw_output_base64: BASE64.encode(&raw),
            provider_call: None,
            failure: None,
            elapsed_ms: 0,
            reasoning_ms: 0,
            inspection_ms: 0,
            validation_ms: 0,
            inspected: vec![],
            inspection_error: None,
            validation_error: None,
            inspected_bytes: 0,
            session_closed_response: Some(SessionClosedResponse {
                exchange_deadline_ms: exchange_deadline,
                admission_deadline_ms: deadline_ms.min(exchange_deadline),
                observed_at_ms: now,
            }),
        };
        save(
            &self.directory.join(format!("{name}.record.json")),
            &serde_jcs::to_vec(&record)?,
        )?;
        Ok(true)
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
        self.reconcile_expired_session_response(search_id, deadline_ms, now)?;
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
            "input_projection_schema":INPUT_PROJECTION_SCHEMA,"input_sha256_scope":"owner_saved_input",
            "configuration_ref":self.configuration_ref,"deadline_ms":deadline_ms,
            "search_elapsed_ms":elapsed,
            "exchanges_used":inputs.len(),"exchanges_remaining":self.budget.max_calls.saturating_sub(inputs.len() as u32),
            "inspection_exchanges_completed":completed_inspections,"inspection_elapsed_ms":inspection_ms,
            "responses_reconciled_after_deadline":records.iter().filter(|(_, _, r)|r.session_closed_response.is_some()).count(),
            "source_identity":inputs.first().map(|(_, _, i)| &i.source_identity),
            "internal_LLM_calls":"unknown","token_usage":"unknown","cost":"unknown",
            "progress":progress,"connected":true,"input":null,"exchange":null});
        if inputs.last().is_some_and(|(name, _, _)| {
            records.iter().any(|(record_name, _, record)| {
                record_name == name && record.session_closed_response.is_some()
            })
        }) {
            view["waiting_reason"] = json!("closed_response_reconciled");
            view["next_operation"] = json!("owner_reconcile_or_assess");
        } else if pending.len() > 1 {
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
                view["input"] = public_reasoning_input(input)?;
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
        fixture_with_agent(None)
    }
    fn fixture_with_agent(
        agent: Option<SessionAgent>,
    ) -> Result<(tempfile::TempDir, Arc<ReasoningProducer>, String)> {
        std::fs::create_dir_all(".tmp")?;
        let root = tempfile::tempdir_in(".tmp")?;
        let producer = Arc::new(ReasoningProducer::new(
            config(agent),
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
    fn projection_fixture(producer: &ReasoningProducer) -> Result<(Vec<u8>, String)> {
        let path = producer.directory.join("r001_s001.input.json");
        let mut input: Value = serde_json::from_slice(&bounded_read(&path, 64 * 1024)?)?;
        let application = format!("sha256:{}", "c".repeat(64));
        input["source_identity"]["closure_ref"] = json!(application);
        // All free-text canaries below are valid owner API metadata. They are
        // synthetic values, not a test that relies on an invalid secret field.
        input["available_variables"] = json!([{"metadata":{
            "requirement":{"name":"OWNER_TOKEN","kind":"service_credential",
                "purpose":"PRIVATE_PURPOSE_CANARY","service":"PRIVATE_SERVICE_CANARY",
                "endpoint":"https://private.invalid/PRIVATE_ENDPOINT_CANARY",
                "account":"PRIVATE_ACCOUNT_CANARY@example.invalid","tenant":"PRIVATE_TENANT_CANARY",
                "resource":"app.http","operation":"execute","phase":"runtime","secret":true,
                "artifact_embedding":false,"temporary":false},
            "applications":[application],"reuse":"this_formation","formation_id":"search_test",
            "expires_at_ms":2000},"revoked_at_ms":null}]);
        let recipe = json!({"schema":"ato.source-oci-recipe/1","dockerfile":"Dockerfile",
            "platform":"linux/amd64","base_images":[{"reference":"example.invalid/image:fixed",
                "pinned_digest":format!("sha256:{}","d".repeat(64))}],
            "build":{"memory_bytes":268435456,"cpu_limit_millis":1000,"pids_limit":64},
            "runtime":{"memory_bytes":268435456,"cpu_limit_millis":1000,"pids_limit":64},
            "build_disk_bytes":536870912,"build_timeout_seconds":60,"max_archive_bytes":1048576});
        input["request"]["operation_catalog"]["operations"] = json!([{
            "operation":"execution_plan@1","toolchains":{"node":"22.0.0"},
            "source_oci":recipe,"sources":[]}]);
        let approved_image = format!("formation.oci.image.{}", "d".repeat(64));
        let unapproved_image = format!("formation.oci.image.{}", "e".repeat(64));
        input["runtime_capabilities"] = json!([{"runtime_id":"runtime-test","environments":[{
            "environment_id":"linux-test","facts_ref":format!("sha256:{}","f".repeat(64)),
            "facts":{"os":"linux","arch":"x86_64","runtime.process":"true","runtime.oci":"true",
                "containment":"bwrap+landlock","formation.source_oci.available":"true",
                "formation.source_oci.bound":"true","toolchain.node.22.0.0":"present",
                "toolchain.node.25.0.0":"present","toolchain.root":"/Users/PRIVATE_HOST_PATH_CANARY",
                "toolchain.private_label":"PRIVATE_TOOLCHAIN_CANARY",
                "formation.source_oci.private_grant":"formation-variable:PRIVATE_GRANT_CANARY",
                (approved_image):"true",(unapproved_image):"true"}}],
            "availability":{"online":true,"observed_at":"PRIVATE_TIMESTAMP_CANARY","capacity":2,
                "current_slots":1,"health":"PRIVATE_HEALTH_CANARY"}}]);
        let source_text = "print('PUBLIC_TARGET_SOURCE_CANARY')\n";
        input["request"]["source_context"] = json!([{"source_id":"src_test","kind":"verified_file",
            "logical_id":"src.test","encoding":"utf8","truncated":false,
            "content_sha256":digest(source_text.as_bytes()),"text":source_text}]);
        let bytes = serde_jcs::to_vec(&input)?;
        let _: ReasoningInput = serde_json::from_slice(&bytes)?;
        replace_public_file(&path, &bytes)?;
        let input_digest = digest(&bytes);
        replace_public_file(
            &producer.directory.join("r001_s001.window.json"),
            &serde_jcs::to_vec(&json!({"exchange_id":"search_test_r1_s1",
                "input_sha256":input_digest,"deadline_ms":1000}))?,
        )?;
        Ok((bytes, input_digest))
    }

    #[test]
    fn session_projection_removes_owner_metadata_and_arbitrary_runtime_facts_without_rebinding()
    -> Result<()> {
        let (_root, producer, _) = fixture()?;
        let (saved, input_digest) = projection_fixture(&producer)?;
        let progress = json!({"status":"pending","unresolved_attempts":0});
        let view = producer.session_view("search_test", 1000, 10, &progress, true)?;
        assert_eq!(view["input_projection_schema"], INPUT_PROJECTION_SCHEMA);
        assert_eq!(view["input_sha256_scope"], "owner_saved_input");
        assert_eq!(view["exchange"]["input_sha256"], input_digest);
        let public = serde_json::to_string(&view)?;
        assert!(!public.contains("PRIVATE_"));
        assert!(public.contains("PUBLIC_TARGET_SOURCE_CANARY"));
        let available = &view["input"]["available_variables"][0];
        assert_eq!(available["metadata"]["requirement"]["name"], "OWNER_TOKEN");
        assert!(available["metadata"]["formation_id"].is_null());
        assert!(available["metadata"]["applications"].is_null());
        let facts = &view["input"]["runtime_capabilities"][0]["environments"][0]["facts"];
        assert_eq!(facts["toolchain.node.22.0.0"], "present");
        assert_eq!(
            facts[format!("formation.oci.image.{}", "d".repeat(64))],
            "true"
        );
        assert!(facts["toolchain.node.25.0.0"].is_null());
        assert!(facts[format!("formation.oci.image.{}", "e".repeat(64))].is_null());
        assert_eq!(
            view["input"]["runtime_capabilities"][0]["availability"]["health"],
            "unknown"
        );
        assert_ne!(digest(&serde_jcs::to_vec(&view["input"])?), input_digest);
        assert_eq!(
            view,
            producer.session_view("search_test", 1000, 10, &progress, true)?
        );
        assert_eq!(
            bounded_read(&producer.directory.join("r001_s001.input.json"), 64 * 1024)?,
            saved
        );
        assert!(String::from_utf8(saved.clone())?.contains("PRIVATE_ACCOUNT_CANARY"));
        // Submission uses the advertised owner-saved digest, never a hash of
        // the projection. Valid typed output continues to bind to that input.
        let reply = producer.submit_session_response(
            "search_test",
            "search_test_r1_s1",
            &input_digest,
            &output(),
            ResponseWindow {
                deadline_ms: 1000,
                now_ms: 10,
                accepting: true,
            },
        )?;
        assert_eq!(reply["response_saved"], true);
        assert_eq!(
            bounded_read(&producer.directory.join("r001_s001.input.json"), 64 * 1024)?,
            saved
        );
        Ok(())
    }

    #[test]
    fn codex_and_claude_receive_identical_projection_for_same_saved_input() -> Result<()> {
        let mut projections = Vec::new();
        for kind in [SessionAgentKind::Codex, SessionAgentKind::ClaudeCode] {
            let (_root, producer, _) = fixture_with_agent(Some(SessionAgent {
                kind,
                version: "fixture-version".into(),
                model: None,
            }))?;
            let (_, input_digest) = projection_fixture(&producer)?;
            let view = producer.session_view(
                "search_test",
                1000,
                10,
                &json!({"status":"pending","unresolved_attempts":0}),
                true,
            )?;
            assert_eq!(view["exchange"]["input_sha256"], input_digest);
            projections.push(view["input"].clone());
        }
        assert_eq!(projections[0], projections[1]);
        Ok(())
    }

    #[test]
    fn unknown_malformed_and_out_of_scope_metadata_is_not_projected() -> Result<()> {
        let (_root, producer, _) = fixture()?;
        let (saved, _) = projection_fixture(&producer)?;
        let mut input: ReasoningInput = serde_json::from_slice(&saved)?;
        let valid_variable = input.available_variables[0].clone();
        let mut unknown = valid_variable.clone();
        unknown["private_grant"] = json!("PRIVATE_UNKNOWN_GRANT_CANARY");
        let mut malformed = valid_variable.clone();
        malformed["metadata"]["requirement"]["secret"] = json!("true");
        let mut unscoped = valid_variable.clone();
        unscoped["metadata"]["formation_id"] = json!("other_search");
        let mut other_application = valid_variable.clone();
        other_application["metadata"]["applications"] =
            json!([format!("sha256:{}", "e".repeat(64))]);
        input.available_variables = vec![unknown, malformed, unscoped, other_application];
        let valid_runtime = input.runtime_capabilities[0].clone();
        let mut unknown_runtime = valid_runtime.clone();
        unknown_runtime["owner_user_id"] = json!("PRIVATE_UNKNOWN_OWNER_CANARY");
        let mut malformed_runtime = valid_runtime.clone();
        malformed_runtime["availability"]["capacity"] = json!(1025);
        input.runtime_capabilities = vec![unknown_runtime, malformed_runtime];
        let public = public_reasoning_input(&input)?;
        assert_eq!(public["available_variables"], json!([]));
        assert_eq!(public["runtime_capabilities"], json!([]));
        // A malformed catalog cannot authorize an arbitrary exact toolchain.
        input.runtime_capabilities = vec![valid_runtime];
        if let ato_formation::proposal::OperationDomain::ExecutionPlan { toolchains, .. } =
            &mut input.request.operation_catalog.operations[0]
        {
            toolchains.insert("PRIVATE_TOOLCHAIN_CANARY".into(), "22.0.0".into());
        }
        let facts =
            &public_reasoning_input(&input)?["runtime_capabilities"][0]["environments"][0]["facts"];
        assert!(facts["toolchain.node.22.0.0"].is_null());
        assert_eq!(facts["runtime.process"], "true");
        Ok(())
    }
    #[test]
    fn saved_response_crossing_poll_deadline_is_receipted_without_admission() -> Result<()> {
        let (_root, producer, input_digest) = fixture()?;
        let bytes = bounded_read(&producer.directory.join("r001_s001.input.json"), 64 * 1024)?;
        let input: ReasoningInput = serde_json::from_slice(&bytes)?;
        let mut times = [998_u64, 1001].into_iter();
        let error = producer
            .wait_session_response(
                "r001_s001",
                &bytes,
                &input,
                1000,
                || times.next().context("unexpected poll"),
                || {
                    producer
                        .submit_session_response(
                            "search_test",
                            "search_test_r1_s1",
                            &input_digest,
                            &output(),
                            ResponseWindow {
                                deadline_ms: 1000,
                                now_ms: 999,
                                accepting: true,
                            },
                        )
                        .unwrap();
                },
            )
            .err()
            .context("expired response must not become a RoundAnswer")?;
        assert!(error.is::<ReasoningSessionDeadline>());
        let records = producer.records()?;
        assert_eq!(records.len(), 1);
        let record = &records[0].2;
        assert_eq!(record.input_sha256, input_digest);
        assert_eq!(record.output_sha256, digest(output().get().as_bytes()));
        assert_eq!(
            record
                .session_closed_response
                .as_ref()
                .unwrap()
                .observed_at_ms,
            1001
        );
        assert!(record.provider_call.is_none());
        assert!(record.inspected.is_empty());
        assert_eq!(record.inspected_bytes, 0);
        assert!(!producer.directory.join("r001_s002.input.json").exists());
        assert_eq!(producer.accounting()?["API_calls"], 0);
        Ok(())
    }

    #[test]
    fn expired_saved_response_reconciles_on_reconnect_once_with_same_exchange_budget() -> Result<()>
    {
        let (root, producer, input_digest) = fixture()?;
        producer.submit_session_response(
            "search_test",
            "search_test_r1_s1",
            &input_digest,
            &output(),
            ResponseWindow {
                deadline_ms: 2000,
                now_ms: 999,
                accepting: true,
            },
        )?;
        drop(producer);
        let restored =
            ReasoningProducer::new(config(None), plan(), root.path().join("reasoning"), None)?;
        let progress = json!({"status":"running","unresolved_attempts":0});
        let view = restored.session_view("search_test", 2000, 1001, &progress, true)?;
        assert_eq!(view["waiting_reason"], "closed_response_reconciled");
        assert!(view["input"].is_null());
        assert_eq!(view["exchanges_used"], 1);
        assert_eq!(view["exchanges_remaining"], 5);
        assert_eq!(view["responses_reconciled_after_deadline"], 1);
        let path = restored.directory.join("r001_s001.record.json");
        let first_receipt = bounded_read(&path, 32 * 1024)?;
        restored.session_view("search_test", 2000, 1500, &progress, false)?;
        assert_eq!(bounded_read(&path, 32 * 1024)?, first_receipt);
        // A lost submit ACK still accepts only exactly the already-saved response.
        restored.submit_session_response(
            "search_test",
            "search_test_r1_s1",
            &input_digest,
            &output(),
            ResponseWindow {
                deadline_ms: 2000,
                now_ms: 1500,
                accepting: false,
            },
        )?;
        let conflicting =
            serde_json::value::to_raw_value(&json!({"schema":"ato.formation-proposal/1",
            "proposals":[{"kind":"unsupported","reason":"no_progress"}]}))?;
        assert!(
            restored
                .submit_session_response(
                    "search_test",
                    "search_test_r1_s1",
                    &input_digest,
                    &conflicting,
                    ResponseWindow {
                        deadline_ms: 2000,
                        now_ms: 1500,
                        accepting: false
                    },
                )
                .is_err()
        );
        assert_eq!(bounded_read(&path, 32 * 1024)?, first_receipt);
        assert!(!restored.directory.join("r001_s002.input.json").exists());
        Ok(())
    }

    #[test]
    fn expired_input_without_response_creates_no_evidence_or_exchange() -> Result<()> {
        let (_root, producer, _) = fixture()?;
        let before = std::fs::read_dir(&producer.directory)?.count();
        assert!(!producer.reconcile_expired_session_response("search_test", 1000, 1001)?);
        let bytes = bounded_read(&producer.directory.join("r001_s001.input.json"), 64 * 1024)?;
        let input: ReasoningInput = serde_json::from_slice(&bytes)?;
        let error = producer
            .wait_session_response(
                "r001_s001",
                &bytes,
                &input,
                1000,
                || Ok(1001),
                || panic!("expired wait must not sleep"),
            )
            .err()
            .context("deadline must stop wait")?;
        assert!(error.is::<ReasoningSessionDeadline>());
        assert_eq!(std::fs::read_dir(&producer.directory)?.count(), before);
        assert!(producer.records()?.is_empty());
        Ok(())
    }

    #[test]
    fn concurrent_closed_reconciliation_is_once_and_detects_later_response_changes() -> Result<()> {
        let (_root, producer, input_digest) = fixture()?;
        producer.submit_session_response(
            "search_test",
            "search_test_r1_s1",
            &input_digest,
            &output(),
            ResponseWindow {
                deadline_ms: 1000,
                now_ms: 999,
                accepting: true,
            },
        )?;
        let left = producer.clone();
        let right = producer.clone();
        let first = std::thread::spawn(move || {
            left.reconcile_expired_session_response("search_test", 1000, 1001)
        });
        let second = std::thread::spawn(move || {
            right.reconcile_expired_session_response("search_test", 1000, 1002)
        });
        assert_eq!(
            usize::from(first.join().unwrap()?) + usize::from(second.join().unwrap()?),
            1
        );
        let record_path = producer.directory.join("r001_s001.record.json");
        let recorded = bounded_read(&record_path, 32 * 1024)?;
        let response_path = producer.directory.join("r001_s001.response.json");
        let mut changed: Value = serde_json::from_slice(&bounded_read(&response_path, 20 * 1024)?)?;
        changed["output"]["proposals"][0]["reason"] = json!("no_progress");
        std::fs::write(response_path, serde_jcs::to_vec(&changed)?)?;
        assert!(
            producer
                .reconcile_expired_session_response("search_test", 1000, 1003)
                .is_err()
        );
        assert_eq!(bounded_read(&record_path, 32 * 1024)?, recorded);
        Ok(())
    }

    #[test]
    fn closed_reconciliation_rejects_stale_tampered_and_windowless_evidence() -> Result<()> {
        for mutation in [
            "digest",
            "exchange",
            "window",
            "duplicate",
            "multiple_pending",
        ] {
            let (_root, producer, input_digest) = fixture()?;
            producer.submit_session_response(
                "search_test",
                "search_test_r1_s1",
                &input_digest,
                &output(),
                ResponseWindow {
                    deadline_ms: 1000,
                    now_ms: 999,
                    accepting: true,
                },
            )?;
            let response_path = producer.directory.join("r001_s001.response.json");
            match mutation {
                "window" => std::fs::remove_file(producer.directory.join("r001_s001.window.json"))?,
                "multiple_pending" => {
                    let mut input: Value = serde_json::from_slice(&bounded_read(
                        &producer.directory.join("r001_s001.input.json"),
                        64 * 1024,
                    )?)?;
                    input["call_id"] = json!("search_test_r1_s2");
                    save(
                        &producer.directory.join("r001_s002.input.json"),
                        &serde_jcs::to_vec(&input)?,
                    )?;
                }
                "duplicate" => std::fs::write(
                    &response_path,
                    format!(
                        r#"{{"schema":"{}","input_sha256":"{}","exchange_id":"search_test_r1_s1","output":{{"schema":"ato.formation-proposal/1","proposals":[{{"kind":"unsupported","reason":"no_progress","reason":"insufficient_source"}}]}}}}"#,
                        SESSION_RESPONSE_SCHEMA, input_digest
                    ),
                )?,
                _ => {
                    let mut response: Value =
                        serde_json::from_slice(&bounded_read(&response_path, 20 * 1024)?)?;
                    response[if mutation == "digest" {
                        "input_sha256"
                    } else {
                        "exchange_id"
                    }] = json!("stale");
                    std::fs::write(&response_path, serde_jcs::to_vec(&response)?)?;
                }
            }
            let result = producer.reconcile_expired_session_response("search_test", 1000, 1001);
            if mutation == "multiple_pending" {
                assert!(!result?, "ambiguous pending exchanges must remain blocked");
            } else {
                assert!(result.is_err(), "{mutation} must be rejected");
            }
            assert!(!producer.directory.join("r001_s001.record.json").exists());
            assert!(!producer.directory.join("r001_s002.record.json").exists());
        }
        Ok(())
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
        let public_descriptor = std::fs::read_to_string(&path)?;
        assert!(!public_descriptor.contains("access_token"));
        assert!(!public_descriptor.contains("capability"));
        assert!(!public_descriptor.contains(&BASE64.encode([7; 32])));
        let descriptor_only = root.path().join("descriptor-only.json");
        std::fs::write(&descriptor_only, &public_descriptor)?;
        assert!(request(&descriptor_only, Command::Status).is_err());
        let wrong_scope = root.path().join("wrong-scope.json");
        let mut changed: Value = serde_json::from_str(&public_descriptor)?;
        changed["search_id"] = json!("search_other");
        std::fs::write(&wrong_scope, serde_json::to_vec(&changed)?)?;
        std::fs::copy(
            path.with_extension("capability"),
            wrong_scope.with_extension("capability"),
        )?;
        assert!(read_authorized_connection(&wrong_scope).is_err());
        let mut malformed = serde_json::to_value(read_connection(&path)?)?;
        malformed["agent"] = json!({"kind":private,"version":"fixture"});
        let malformed_path = root.path().join("malformed.json");
        replace_public_file(&malformed_path, &serde_jcs::to_vec(&malformed)?)?;
        assert!(
            !format!("{:#}", read_connection(&malformed_path).err().unwrap()).contains(private)
        );
        let wrong = read_connection(&path)?;
        let forged = root.path().join("forged.json");
        replace_public_file(&forged, &serde_jcs::to_vec(&wrong)?)?;
        publish_capability(&forged, &wrong, [8; 32])?;
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
