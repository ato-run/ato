use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct Envelope {
    pub spec_version: String,
}

#[derive(Debug)]
pub struct InternalArgs {
    pub input: String,
    pub command: InternalCommand,
}

#[derive(Debug)]
pub enum InternalCommand {
    Features,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ErrorBody {
    code: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct Response<T> {
    ok: bool,
    spec_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(flatten)]
    data: Option<T>,
}

#[derive(Debug, Serialize)]
struct FeaturesData {
    engine: EngineInfo,
    capabilities: Capabilities,
}

#[derive(Debug, Serialize)]
struct EngineInfo {
    name: String,
    engine_version: String,
    platform: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit: Option<String>,
}

#[derive(Debug, Serialize)]
struct Capabilities {
    workloads: Vec<String>,
    languages: Vec<String>,
    sandbox: Vec<String>,
    socket_activation: bool,
    jit_provisioning: bool,
}

pub async fn execute(args: InternalArgs) -> Result<()> {
    // Internal interface must keep stdout machine-clean (JSON only).
    // Mark internal mode so shared helpers can route progress/logs to stderr.
    std::env::set_var("NACELLE_INTERNAL", "1");

    match args.command {
        InternalCommand::Features => handle_features(args.input).await,
    }
}

async fn handle_features(input: String) -> Result<()> {
    let spec_version = parse_spec_version(&input).unwrap_or_else(|| "0.1.0".to_string());

    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let commit = std::env::var("GIT_COMMIT").ok();

    #[cfg(target_os = "macos")]
    let sandbox = vec!["macos-seatbelt".to_string()];
    #[cfg(target_os = "linux")]
    let sandbox = vec!["linux-landlock".to_string()];
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let sandbox: Vec<String> = Vec::new();

    let data = FeaturesData {
        engine: EngineInfo {
            name: "nacelle".to_string(),
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
            platform,
            commit,
        },
        capabilities: Capabilities {
            workloads: vec!["source".to_string(), "bundle".to_string()],
            languages: vec!["python".to_string()],
            sandbox,
            socket_activation: true,
            jit_provisioning: true,
        },
    };

    write_ok(spec_version, data);
    Ok(())
}

fn parse_spec_version(input: &str) -> Option<String> {
    let raw = read_input(input).ok()?;
    let env: Envelope = serde_json::from_str(&raw).ok()?;
    Some(env.spec_version)
}

fn read_input(input: &str) -> Result<String> {
    if input == "-" {
        let mut buf = String::new();
        let mut stdin = std::io::stdin();
        stdin
            .read_to_string(&mut buf)
            .context("Failed to read stdin")?;
        if buf.trim().is_empty() {
            return Ok("{}".to_string());
        }
        return Ok(buf);
    }

    let path = PathBuf::from(input);
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read input file: {}", path.display()))?;
    Ok(content)
}

fn write_ok<T: Serialize>(spec_version: String, data: T) {
    let resp = Response {
        ok: true,
        spec_version,
        error: None,
        data: Some(data),
    };
    println!("{}", serde_json::to_string(&resp).unwrap());
}
