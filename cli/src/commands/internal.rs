use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::PathBuf;

use super::{dev, pack_v2};

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
    Pack,
    Exec,
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

#[derive(Debug, Deserialize)]
struct PackRequest {
    spec_version: String,
    workload: Workload,
    output: PackOutput,
    #[serde(default)]
    runtime_path: Option<PathBuf>,
    #[serde(default)]
    options: PackOptions,
}

#[derive(Debug, Default, Deserialize)]
struct PackOptions {
    #[serde(default)]
    sign: bool,
}

#[derive(Debug, Deserialize)]
struct PackOutput {
    format: String,
    #[serde(default)]
    path: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct ExecRequest {
    spec_version: String,
    workload: Workload,
    #[serde(default)]
    interactive: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct Workload {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(default)]
    manifest: Option<PathBuf>,
    #[serde(default)]
    entrypoint: Option<String>,
}

#[derive(Debug, Serialize)]
struct PackData {
    artifact: Artifact,
}

#[derive(Debug, Serialize)]
struct Artifact {
    format: String,
    path: String,
}

#[derive(Debug, Serialize)]
struct ExecData {
    result: ExecResult,
}

#[derive(Debug, Serialize)]
struct ExecResult {
    status: String,
    exit_code: i32,
    pid: u32,
}

pub async fn execute(args: InternalArgs) -> Result<()> {
    // Internal interface must keep stdout machine-clean (JSON only).
    // Mark internal mode so shared helpers can route progress/logs to stderr.
    std::env::set_var("NACELLE_INTERNAL", "1");

    match args.command {
        InternalCommand::Features => handle_features(args.input).await,
        InternalCommand::Pack => handle_pack(args.input).await,
        InternalCommand::Exec => handle_exec(args.input).await,
    }
}

async fn handle_features(input: String) -> Result<()> {
    let spec_version = parse_spec_version(&input).unwrap_or_else(|| "0.1.0".to_string());

    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let commit = std::env::var("GIT_COMMIT").ok();

    let mut sandbox = Vec::new();
    #[cfg(target_os = "macos")]
    sandbox.push("macos-seatbelt".to_string());
    #[cfg(target_os = "linux")]
    sandbox.push("linux-landlock".to_string());

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

async fn handle_pack(input: String) -> Result<()> {
    let req: PackRequest = serde_json::from_str(&read_input(&input)?)
        .with_context(|| "Failed to parse JSON input for internal pack")?;

    if req.workload.kind != "source" {
        write_error(
            req.spec_version,
            "UNSUPPORTED",
            format!("Unsupported workload.type: {}", req.workload.kind),
            None,
        );
        std::process::exit(2);
    }

    if req.output.format != "bundle" {
        write_error(
            req.spec_version,
            "UNSUPPORTED",
            format!("Unsupported output.format: {}", req.output.format),
            None,
        );
        std::process::exit(2);
    }

    if req.options.sign {
        eprintln!("⚠️  internal pack: options.sign is currently ignored");
    }

    let manifest_path = req
        .workload
        .manifest
        .clone()
        .or_else(|| req.workload.path.as_ref().map(|p| p.join("capsule.toml")))
        .context("workload.manifest is required (or workload.path with capsule.toml)")?;

    let output_path = req.output.path.clone();

    let bundle_path = pack_v2::build_bundle(pack_v2::PackV2Args {
        manifest_path,
        runtime_path: req.runtime_path.clone(),
        output: output_path,
    })
    .await
    .context("Failed to build bundle")?;

    write_ok(
        req.spec_version,
        PackData {
            artifact: Artifact {
                format: "bundle".to_string(),
                path: bundle_path.display().to_string(),
            },
        },
    );
    Ok(())
}

async fn handle_exec(input: String) -> Result<()> {
    let req: ExecRequest = serde_json::from_str(&read_input(&input)?)
        .with_context(|| "Failed to parse JSON input for internal exec")?;

    if req.workload.kind != "source" {
        write_error(
            req.spec_version,
            "UNSUPPORTED",
            format!("Unsupported workload.type: {}", req.workload.kind),
            None,
        );
        std::process::exit(2);
    }

    let manifest_path = req
        .workload
        .manifest
        .clone()
        .or_else(|| req.workload.path.as_ref().map(|p| p.join("capsule.toml")))
        .context("workload.manifest is required (or workload.path with capsule.toml)")?;

    if req.interactive {
        // Interactive/streaming: run in the foreground and rely on stdio inheritance.
        // Do not emit JSON to stdout (it would pollute logs). Exit code is surfaced
        // via this process' exit status.
        let outcome = dev::run_streaming(manifest_path)
            .await
            .context("internal exec failed")?;

        let exit_code = outcome
            .exit_status
            .as_ref()
            .and_then(|s| s.code())
            .unwrap_or(0);

        std::process::exit(exit_code);
    }

    // Non-interactive JSON mode (RPC-like).
    let outcome = dev::run_non_interactive(manifest_path)
        .await
        .context("internal exec failed")?;

    let exit_code = outcome
        .exit_status
        .as_ref()
        .and_then(|s| s.code())
        .unwrap_or(1);

    write_ok(
        req.spec_version,
        ExecData {
            result: ExecResult {
                status: "exited".to_string(),
                exit_code,
                pid: outcome.pid,
            },
        },
    );

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
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

fn write_error(
    spec_version: String,
    code: &str,
    message: String,
    details: Option<serde_json::Value>,
) {
    let resp: Response<serde_json::Value> = Response {
        ok: false,
        spec_version,
        error: Some(ErrorBody {
            code: code.to_string(),
            message,
            details,
        }),
        data: None,
    };
    println!("{}", serde_json::to_string(&resp).unwrap());
}
