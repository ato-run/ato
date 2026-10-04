//! Owner-only entry point over the common Source Result/functional Run API.
use anyhow::{Context, Result, ensure};
use ato_formation::{
    functional_acceptance::FunctionalAcceptanceV1, requirements::ExecutionRequirements,
};
use ato_formation_worker::runtime_network::{Client, SatisfyBudget};
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Debug, Args)]
pub(super) struct VerifyArgs {
    #[arg(long)]
    source_search_id: String,
    #[arg(long, env = "ATO_RUNTIME_NETWORK_API")]
    api: String,
    /// Owning account's session token. Runtime/Bridge capabilities cannot grant permission.
    #[arg(long)]
    owner_token_file: PathBuf,
    /// A saved bounded typed plan with stable request/registration/Search IDs.
    #[arg(long)]
    plan: PathBuf,
    /// Approve only this plan's functional verification; never an ordinary Run or publish.
    #[arg(long, required = true)]
    authorize_functional_verification: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    request_id: String,
    registration_request_id: String,
    search_id: String,
    runtime_id: String,
    environment_id: String,
    target_triple: String,
    permission: String,
    budget: SatisfyBudget,
    ceiling: ExecutionRequirements,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    acceptance: Option<FunctionalAcceptanceV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_result_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retained_ref: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceResultView {
    schema: String,
    source_result_ref: String,
    search_id: String,
    attempt_id: String,
    source_closure_ref: String,
    contract_ref: String,
    derivation_ref: String,
    retained_ref: String,
    runtime_id: String,
    environment_id: String,
    capability_profile_ref: String,
    target_triple: String,
    permission: String,
}
fn reference(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|b| (0x21..=0x7e).contains(&b))
}
impl Plan {
    fn bind(&mut self, source: &SourceResultView, origin: &str) -> Result<()> {
        ensure!(
            self.permission == "functional_verification",
            "functional permission required"
        );
        ensure!(
            source.schema == "ato.formation-source-result-view/1"
                && source.permission == "functional_verification_required"
                && source.search_id == origin,
            "Source Result scope mismatch"
        );
        ensure!(
            [
                &source.source_result_ref,
                &source.source_closure_ref,
                &source.contract_ref,
                &source.derivation_ref,
                &source.retained_ref,
                &source.capability_profile_ref
            ]
            .into_iter()
            .all(|r| reference(r)),
            "Source Result reference invalid"
        );
        ensure!(
            self.search_id != origin
                && identifier(&self.search_id)
                && self
                    .search_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
            "functional Search must be fresh"
        );
        ensure!(
            [
                &self.request_id,
                &self.registration_request_id,
                &self.runtime_id,
                &self.environment_id
            ]
            .into_iter()
            .all(|s| identifier(s)),
            "functional identity invalid"
        );
        ensure!(
            matches!(
                self.target_triple.as_str(),
                "x86_64-unknown-linux-gnu" | "aarch64-unknown-linux-gnu"
            ) && self.target_triple == source.target_triple,
            "saved Source target mismatch"
        );
        ensure!(
            self.source_result_ref
                .as_ref()
                .is_none_or(|r| r == &source.source_result_ref)
                && self
                    .retained_ref
                    .as_ref()
                    .is_none_or(|r| r == &source.retained_ref),
            "saved Source Result changed"
        );
        self.budget.validate()?;
        ensure!(
            self.budget.max_attempts == 1 && self.budget.mode == "first_pass",
            "one functional attempt required"
        );
        self.ceiling.validate()?;
        self.source_result_ref = Some(source.source_result_ref.clone());
        self.retained_ref = Some(source.retained_ref.clone());
        Ok(())
    }
}
fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "owner input exceeds size limit"
    );
    Ok(bytes)
}
fn prepare(client: &Client, origin: &str, mut plan: Plan) -> Result<Value> {
    let source: SourceResultView = serde_json::from_value(client.source_result(origin)?)
        .map_err(|_| anyhow::anyhow!("invalid Source Result view"))?;
    plan.bind(&source, origin)?;
    let result = client.prepare_retained_verification(origin, &serde_json::to_value(plan)?)?;
    // Only known saved identities/status leave this owner command. No private
    // assignment, Runtime ticket, credential, endpoint or transport capability.
    let mut metadata = serde_json::Map::new();
    for name in [
        "registration_id",
        "compute_instance_id",
        "compute_schema_id",
        "search_id",
        "run_id",
        "lease_id",
        "satisfy_id",
        "status",
        "ack",
    ] {
        let value = result
            .get(name)
            .context("functional result metadata missing")?;
        ensure!(
            value.is_null() || value.as_str().is_some_and(|s| s.len() <= 256),
            "functional result metadata invalid"
        );
        metadata.insert(name.to_owned(), value.clone());
    }
    Ok(
        json!({"schema":"ato.formation-functional-preparation/1", "source":source,
        "functional":metadata, "permission":"functional_verification"}),
    )
}
pub(super) fn run(args: VerifyArgs) -> Result<()> {
    ensure!(
        args.authorize_functional_verification,
        "explicit functional verification approval required"
    );
    let token =
        read_bounded(&args.owner_token_file, 16384).context("read owning account session token")?;
    let token = std::str::from_utf8(&token).context("owner session token encoding")?;
    ensure!(!token.trim().is_empty(), "owner session token is empty");
    let bytes = read_bounded(&args.plan, 32768).context("read saved functional plan")?;
    let plan: Plan = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid typed functional plan"))?;
    let result = prepare(
        &Client::new(&args.api, token)?,
        &args.source_search_id,
        plan,
    )?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    fn source() -> SourceResultView {
        serde_json::from_value(json!({"schema":"ato.formation-source-result-view/1",
          "source_result_ref":format!("sha256:{}", "a".repeat(64)),"search_id":"source-search","attempt_id":"attempt",
          "source_closure_ref":format!("sha256:{}", "b".repeat(64)),"contract_ref":format!("sha256:{}", "c".repeat(64)),
          "derivation_ref":format!("sha256:{}", "d".repeat(64)),"retained_ref":format!("sha256:{}", "e".repeat(64)),
          "runtime_id":"runtime","environment_id":"native","capability_profile_ref":format!("sha256:{}", "f".repeat(64)),
          "target_triple":"aarch64-unknown-linux-gnu","permission":"functional_verification_required"})).unwrap()
    }
    fn plan() -> Plan {
        serde_json::from_value(json!({"request_id":"run-a","registration_request_id":"same-registration", "search_id":"functional-a",
          "runtime_id":"runtime","environment_id":"native","target_triple":"aarch64-unknown-linux-gnu", "permission":"functional_verification",
          "budget":{"max_attempts":1,"mode":"first_pass","deadline_seconds":60,"max_transfer_bytes":4096,"max_expanded_bytes":4096,"max_stored_bytes":4096},"ceiling":{}})).unwrap()
    }
    #[test]
    fn permission_and_immutable_source_bindings_are_required() {
        let mut p = plan();
        p.bind(&source(), "source-search").unwrap();
        let original = serde_json::to_value(&p).unwrap();
        p.bind(&source(), "source-search").unwrap();
        assert_eq!(original, serde_json::to_value(&p).unwrap());
        let mut changed = source();
        changed.retained_ref = format!("sha256:{}", "1".repeat(64));
        assert!(p.bind(&changed, "source-search").is_err());
        p = plan();
        p.search_id = "source-search".into();
        assert!(p.bind(&source(), "source-search").is_err());
        p = plan();
        p.permission = "run".into();
        assert!(p.bind(&source(), "source-search").is_err());
        p = plan();
        p.budget.max_attempts = 2;
        assert!(p.bind(&source(), "source-search").is_err());
        p = plan();
        p.target_triple = "x86_64-unknown-linux-gnu".into();
        assert!(p.bind(&source(), "source-search").is_err());
        let mut extra = serde_json::to_value(plan()).unwrap();
        extra["source_instance_id"] = json!("fake");
        assert!(serde_json::from_value::<Plan>(extra).is_err());
    }
    #[test]
    fn cli_requires_owner_approval_and_a_saved_plan() {
        let args = [
            "ato",
            "form-verify",
            "--source-search-id",
            "origin",
            "--api",
            "https://api.test",
            "--owner-token-file",
            "owner-session",
            "--plan",
            "plan.json",
        ];
        assert!(crate::Cli::try_parse_from(args).is_err());
        assert!(
            crate::Cli::try_parse_from(
                args.into_iter()
                    .chain(["--authorize-functional-verification"])
            )
            .is_ok()
        );
    }
    #[test]
    fn common_http_client_posts_saved_plan_and_exports_only_metadata() {
        use std::io::Write;
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let source_json = serde_json::to_string(&source()).unwrap();
        let worker = std::thread::spawn(move || {
            let mut submitted = None;
            for (index, body) in [source_json, json!({"registration_id":"registration", "compute_instance_id":"instance", "compute_schema_id":"schema", "search_id":"functional-a", "run_id":"saved-run", "lease_id":"saved-lease",
                "satisfy_id":"saved-request", "status":"running", "ack":"unknown", "ticket":"private-canary"}).to_string()].into_iter().enumerate() {
                let (mut socket, _) = listener.accept().unwrap();
                socket.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                let mut bytes = Vec::new();
                let headers_end = loop {
                    let mut byte = [0]; socket.read_exact(&mut byte).unwrap(); bytes.push(byte[0]);
                    if bytes.ends_with(b"\r\n\r\n") { break bytes.len(); }
                    assert!(bytes.len() < 8192);
                };
                let header = std::str::from_utf8(&bytes).unwrap();
                assert!(header.to_ascii_lowercase().contains("authorization: bearer owner-test"));
                if index == 0 {
                    assert!(header.starts_with("GET /v1/runtime-network/exploration/source-search/source-result "));
                } else {
                    assert!(header.starts_with("POST /v1/runtime-network/exploration/source-search/retained-verification "));
                    let length: usize = header.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ").map(|v| v.trim().parse().unwrap())).unwrap();
                    bytes.resize(headers_end + length, 0); socket.read_exact(&mut bytes[headers_end..]).unwrap();
                    submitted = Some(serde_json::from_slice::<Value>(&bytes[headers_end..]).unwrap());
                }
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
            submitted.unwrap()
        });
        let result = prepare(
            &Client::new(&api, "owner-test").unwrap(),
            "source-search",
            plan(),
        )
        .unwrap();
        let submitted = worker.join().unwrap();
        assert_eq!(submitted["registration_request_id"], "same-registration");
        assert_eq!(submitted["retained_ref"], source().retained_ref);
        assert_eq!(submitted["source_result_ref"], source().source_result_ref);
        assert!(submitted.get("source_instance_id").is_none());
        assert_eq!(result["functional"]["run_id"], "saved-run");
        assert!(!result.to_string().contains("private-canary"));
    }
}
