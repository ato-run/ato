//! Optional local Model Set installation.
//!
//! A Model Set is downloaded only on explicit request, and every byte is pinned
//! to one upstream revision by byte count and sha256 before it becomes usable.
//! The payload lives under ATO_HOME/models/<set>/; nothing is written into a
//! Capsule, an Instance or the repository. Inference itself is a separate
//! runtime concern and never runs from this module.
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// One pinned file of a Model Set.
///
/// "url" is the exact upstream resolve URL. "sha256" is the upstream-published
/// LFS object id for LFS payloads and the digest of the immutable repository
/// blob (addressed by "revision") for metadata files.
#[derive(Clone, Debug)]
pub struct Asset {
    pub path: &'static str,
    pub role: &'static str,
    pub url: String,
    pub sha256: &'static str,
    pub bytes: u64,
}

const KEV_SET: &str = "kev-0.8b";
const KEV_REPOSITORY: &str = "jaredpalmer/kev-0.8b";
const KEV_REVISION: &str = "bf75a6a8848ea6960ff2ed108d9ed44c2941174f";
const KEV_BASE_REPOSITORY: &str = "Qwen/Qwen3.5-0.8B-Base";
const KEV_BASE_REVISION: &str = "dc7cdfe2ee4154fa7e30f5b51ca41bfa40174e68";
const KEV_LICENSE: &str = "Apache-2.0";
const INSTALL_SCHEMA: &str = "ato.model-install/1";
const STATUS_SCHEMA: &str = "ato.model-status/1";

#[derive(Subcommand)]
pub enum ModelCommands {
    /// Download, verify and record the optional assets of a named Model Set.
    Install(InstallArgs),
    /// Report which pinned Model Set assets are present on this host.
    Status(StatusArgs),
    /// Delete the locally installed assets of a named Model Set.
    Remove(RemoveArgs),
    /// Score one state against typed questions with the locally installed Model Set.
    Score(ScoreArgs),
}

#[derive(Args)]
pub struct InstallArgs {
    /// Model Set name.
    #[arg(default_value = "kev-0.8b")]
    pub model: String,
    /// Override ATO_HOME.
    #[arg(long)]
    pub home: Option<PathBuf>,
    /// Print the machine-readable report only.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct StatusArgs {
    /// Model Set name.
    #[arg(default_value = "kev-0.8b")]
    pub model: String,
    /// Override ATO_HOME.
    #[arg(long)]
    pub home: Option<PathBuf>,
    /// Recompute every payload digest instead of trusting size and provenance.
    #[arg(long)]
    pub verify: bool,
    /// Print the machine-readable report only.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct RemoveArgs {
    /// Model Set name.
    #[arg(default_value = "kev-0.8b")]
    pub model: String,
    /// Override ATO_HOME.
    #[arg(long)]
    pub home: Option<PathBuf>,
    /// Print the machine-readable report only.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct ScoreArgs {
    /// Model Set name.
    #[arg(default_value = "kev-0.8b")]
    pub model: String,
    /// JSON request: {"state": <text|json>, "questions": {<id>: <question>}}. "-" reads stdin.
    #[arg(long)]
    pub request: String,
    /// Override ATO_HOME.
    #[arg(long)]
    pub home: Option<PathBuf>,
    /// Interpreter that has the Kev runtime installed. Defaults to ATO_KEV_PYTHON, then python3.
    #[arg(long)]
    pub python: Option<PathBuf>,
    /// Print the machine-readable report only.
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize)]
pub struct ModelSource {
    pub repository: String,
    pub revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<String>,
}

#[derive(Serialize)]
pub struct AssetState {
    pub path: String,
    pub role: String,
    pub bytes: u64,
    pub present: bool,
    /// Some(false) means a present payload is not the pinned bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
}

#[derive(Serialize)]
pub struct ModelStatusReport {
    pub schema: &'static str,
    pub model: String,
    pub installed: bool,
    pub root: String,
    pub license: &'static str,
    pub source: ModelSource,
    pub total_bytes: u64,
    pub installed_bytes: u64,
    pub assets: Vec<AssetState>,
}

#[derive(Serialize)]
pub struct InstalledAsset {
    pub path: String,
    pub role: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Serialize)]
pub struct InstallReport {
    pub schema: &'static str,
    pub model: String,
    pub root: String,
    pub license: &'static str,
    pub source: ModelSource,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub already_present: bool,
}

#[derive(Serialize)]
pub struct RemoveReport {
    pub schema: &'static str,
    pub model: String,
    pub root: String,
    pub removed_bytes: u64,
    pub removed: bool,
}

/// The upstream name of a locally laid-out payload.
///
/// The local layout groups files by role so a runtime can load one directory per
/// role, while every upstream file sits at the top level of its repository. This
/// mapping never decides which bytes are pinned; the pinned digest does.
fn upstream_path(path: &str) -> &str {
    match path {
        // The released Qwen snapshot names its single weight shard this way.
        "base/model.safetensors" => "model.safetensors-00001-of-00001.safetensors",
        other => other.rsplit('/').next().unwrap_or(other),
    }
}

fn resolve(repository: &str, revision: &str, path: &str) -> String {
    format!("https://huggingface.co/{repository}/resolve/{revision}/{path}")
}

/// The pinned asset table. Adding a Model Set means adding one arm here; the
/// installer, status report and removal path stay set-agnostic.
fn assets(model: &str) -> Result<(ModelSource, &'static str, Vec<Asset>)> {
    ensure!(model == KEV_SET, "unknown_model_set: {model}");
    let upstream = upstream_path;
    let base = |path: &'static str, sha256: &'static str, bytes: u64, role: &'static str| Asset {
        path,
        role,
        url: resolve(KEV_BASE_REPOSITORY, KEV_BASE_REVISION, upstream(path)),
        sha256,
        bytes,
    };
    let kev = |path: &'static str, sha256: &'static str, bytes: u64, role: &'static str| Asset {
        path,
        role,
        url: resolve(KEV_REPOSITORY, KEV_REVISION, upstream(path)),
        sha256,
        bytes,
    };
    let assets = vec![
        base(
            "base/config.json",
            "b90b86f35c8e6925ef74ee04d0e758f0a845c83a42089ad82bbaa948de9b4204",
            2907,
            "base",
        ),
        base(
            "base/model.safetensors",
            "c2b1e5a17d9c1e27685d92ed9b382911ebb99955ecd89052d1721241adfbab6c",
            1_746_942_600,
            "base",
        ),
        base(
            "base/model.safetensors.index.json",
            "ce9a885efdf27d3664fdef5d512ad365216f1074051ef840c7cd8e5431495d0a",
            50900,
            "base",
        ),
        base(
            "base/tokenizer.json",
            "fe000e3ed39ed12b8d2481d527d44f93c65d37e87645d2dcc80d1bf9d50d2927",
            12_807_196,
            "base",
        ),
        base(
            "base/tokenizer_config.json",
            "e611fbccc7c29ef3b1cafb1cb7ea548d189968632901d678fd62be68c47885de",
            16712,
            "base",
        ),
        base(
            "base/vocab.json",
            "ce99b4cb2983d118806ce0a8b777a35b093e2000a503ebde25853284c9dfa003",
            6_722_759,
            "base",
        ),
        base(
            "base/merges.txt",
            "a9d356d7bdf1ef4949e3e748e95b8e10ad9d4e2e838eddc38a0a7b6b94d1db8d",
            3_353_259,
            "base",
        ),
        kev(
            "adapter/adapter_config.json",
            "748acb2cda88454cb1ba69d745ba336f3fcb5486eac349e90960c8b8d8d3e854",
            1273,
            "adapter",
        ),
        kev(
            "adapter/adapter_model.safetensors",
            "9b908623acb162118575f4e7a94524f9c139c335be4bfb74d6cfceca01e1885a",
            43_338_624,
            "adapter",
        ),
        kev(
            "tokenizer/tokenizer.json",
            "06b9509352d2af50381ab2247e083b80d32d5c0aba91c272ca9ff729b6a0e523",
            19_989_325,
            "tokenizer",
        ),
        kev(
            "tokenizer/tokenizer_config.json",
            "8671bed7c852ce9e661be94f179a7b4ffd091c2a65aea0363e5501c20318ee45",
            1128,
            "tokenizer",
        ),
        kev(
            "head.pt",
            "f400bd12802b2b105ae45d6b03774a158a3db4fccff42413734ddca2e5c920b6",
            2_103_999,
            "head",
        ),
    ];
    let source = ModelSource {
        repository: KEV_REPOSITORY.to_owned(),
        revision: KEV_REVISION.to_owned(),
        base_repository: Some(KEV_BASE_REPOSITORY.to_owned()),
        base_revision: Some(KEV_BASE_REVISION.to_owned()),
    };
    Ok((source, KEV_LICENSE, assets))
}

fn models_root(home: Option<PathBuf>) -> Result<PathBuf> {
    match home {
        Some(home) => Ok(home.join("models")),
        None => Ok(super::ato_home()?.join("models")),
    }
}

pub fn execute(command: ModelCommands) -> Result<()> {
    match command {
        ModelCommands::Install(args) => install(&args),
        ModelCommands::Status(args) => status(&args),
        ModelCommands::Remove(args) => remove(&args),
        ModelCommands::Score(args) => score(&args),
    }
}

/// The runner that owns the forward pass. It is embedded so the CLI never depends
/// on a checkout of the reference runtime, and it is written next to the payload
/// so a run is reproducible from the installed Model Set alone.
const KEV_RUNNER: &str = include_str!("../assets/kev_runner.py");
const SCORE_SCHEMA: &str = "ato.model-score/1";

fn score(args: &ScoreArgs) -> Result<()> {
    let report = score_request(args)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "{} on {} ({} ms, load {} s)",
            report.model,
            report.runtime.backend,
            report.runtime.inference_ms,
            report.runtime.load_seconds
        );
        for (id, answer) in &report.answers {
            println!("  {id}: {}", serde_json::to_string(answer)?);
        }
    }
    Ok(())
}

fn score_request(args: &ScoreArgs) -> Result<ScoreReport> {
    // Refuse before starting an interpreter: an incomplete Model Set is a
    // download problem, not an inference problem.
    let status = status_report(&args.model, args.home.clone(), false)?;
    ensure!(
        status.installed,
        "model_set_incomplete: {} is missing {} of {} assets; run: ato model install {}",
        args.model,
        status.assets.iter().filter(|asset| !asset.present).count(),
        status.assets.len(),
        args.model
    );
    let request = read_request(&args.request)?;
    let root = PathBuf::from(&status.root);
    let runner = root.join("kev_runner.py");
    fs::write(&runner, KEV_RUNNER).with_context(|| format!("write {}", runner.display()))?;
    let python = args
        .python
        .clone()
        .or_else(|| std::env::var_os("ATO_KEV_PYTHON").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("python3"));
    let payload = serde_json::json!({
        "state": request.state,
        "questions": request.questions,
        "model_root": status.root,
    });
    let value = invoke_runner(&python, &runner, &payload)?;
    let answers: BTreeMap<String, serde_json::Value> =
        serde_json::from_value(value.get("answers").cloned().unwrap_or_default())
            .context("runner answers are not an object")?;
    let raw: Vec<Vec<f64>> = serde_json::from_value(value.get("raw").cloned().unwrap_or_default())
        .context("runner probabilities are not a matrix")?;
    let runtime: RuntimeReport =
        serde_json::from_value(value.get("runtime").cloned().unwrap_or_default())
            .context("runner runtime block is malformed")?;
    let checkpoint: CheckpointReport =
        serde_json::from_value(value.get("checkpoint").cloned().unwrap_or_default())
            .context("runner checkpoint block is malformed")?;
    Ok(ScoreReport {
        schema: SCORE_SCHEMA,
        model: args.model.clone(),
        root: status.root,
        answers,
        raw,
        runtime,
        checkpoint,
    })
}

/// Run the embedded runner and return its JSON answer.
///
/// The runner is a separate process on purpose: a model crash, an out-of-memory
/// kill or a missing runtime must not take the CLI down with it, and the CLI must
/// never link a Python interpreter.
fn invoke_runner(
    python: &Path,
    runner: &Path,
    payload: &serde_json::Value,
) -> Result<serde_json::Value> {
    let mut child = std::process::Command::new(python)
        .arg(runner)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("start {}", python.display()))?;
    child
        .stdin
        .take()
        .context("runner stdin is unavailable")?
        .write_all(serde_json::to_string(payload)?.as_bytes())?;
    let output = child.wait_with_output().context("wait for the runner")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).with_context(|| {
        format!(
            "model_runtime_failed: the runner did not answer with JSON (exit {:?})\nstdout: {}\nstderr: {}",
            output.status.code(),
            stdout.trim(),
            stderr.trim()
        )
    })?;
    if let Some(error) = value.get("error") {
        bail!(
            "{}: {}",
            error
                .get("code")
                .and_then(|code| code.as_str())
                .unwrap_or("model_runtime_failed"),
            error
                .get("message")
                .and_then(|message| message.as_str())
                .unwrap_or("the runner reported an error")
        );
    }
    Ok(value)
}

#[derive(Debug)]
struct ScoreRequest {
    state: serde_json::Value,
    questions: serde_json::Value,
}

fn read_request(request: &str) -> Result<ScoreRequest> {
    let text = if request == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .context("read the request from stdin")?;
        buffer
    } else {
        request.to_owned()
    };
    let value: serde_json::Value =
        serde_json::from_str(&text).context("the request is not valid JSON")?;
    let state = value
        .get("state")
        .cloned()
        .context("the request needs a state")?;
    let questions = value
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
    Ok(ScoreRequest { state, questions })
}

#[derive(Serialize, serde::Deserialize)]
pub struct RuntimeReport {
    pub backend: String,
    pub device: String,
    #[serde(default)]
    pub mlx: Option<String>,
    #[serde(default)]
    pub mlx_lm: Option<String>,
    pub python: String,
    pub load_seconds: f64,
    pub inference_ms: f64,
}

#[derive(Serialize, serde::Deserialize)]
pub struct CheckpointReport {
    pub weights: String,
    pub base: String,
    #[serde(default)]
    pub base_revision: Option<String>,
    pub head_dim: u64,
    pub temperature: f64,
}

#[derive(Serialize)]
pub struct ScoreReport {
    pub schema: &'static str,
    pub model: String,
    pub root: String,
    pub answers: BTreeMap<String, serde_json::Value>,
    pub raw: Vec<Vec<f64>>,
    pub runtime: RuntimeReport,
    pub checkpoint: CheckpointReport,
}

fn status(args: &StatusArgs) -> Result<()> {
    let report = status_report(&args.model, args.home.clone(), args.verify)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("model: {} ({})", report.model, report.license);
    println!("root: {}", report.root);
    println!(
        "source: {} @ {}{}",
        report.source.repository,
        report.source.revision,
        match (&report.source.base_repository, &report.source.base_revision) {
            (Some(repository), Some(revision)) => format!(" + {repository} @ {revision}"),
            _ => String::new(),
        }
    );
    for asset in &report.assets {
        let state = match (asset.present, asset.verified) {
            (false, _) => "missing",
            (true, Some(false)) => "CORRUPT",
            (true, Some(true)) => "verified",
            (true, None) => "present",
        };
        println!("  {state:9} {:>12}  {}", asset.bytes, asset.path);
    }
    println!(
        "installed: {} of {} bytes ({})",
        report.installed_bytes,
        report.total_bytes,
        if report.installed {
            "complete"
        } else {
            "incomplete"
        }
    );
    if !report.installed {
        println!("run: ato model install {}", report.model);
    }
    Ok(())
}

fn status_report(model: &str, home: Option<PathBuf>, verify: bool) -> Result<ModelStatusReport> {
    let (source, license, assets) = assets(model)?;
    let root = models_root(home)?.join(model);
    let mut installed_bytes = 0u64;
    let mut total_bytes = 0u64;
    let mut states = Vec::with_capacity(assets.len());
    for asset in &assets {
        total_bytes += asset.bytes;
        let target = root.join(asset.path);
        let present = match fs::metadata(&target) {
            Ok(metadata) => metadata.is_file() && metadata.len() == asset.bytes,
            Err(_) => false,
        };
        if present {
            installed_bytes += asset.bytes;
        }
        let verified = if verify && present {
            Some(file_digest(&target)? == asset.sha256)
        } else {
            None
        };
        states.push(AssetState {
            path: asset.path.to_owned(),
            role: asset.role.to_owned(),
            bytes: asset.bytes,
            present: present && verified != Some(false),
            verified,
        });
    }
    let installed = states.iter().all(|asset| asset.present);
    Ok(ModelStatusReport {
        schema: STATUS_SCHEMA,
        model: model.to_owned(),
        installed,
        root: root.display().to_string(),
        license,
        source,
        total_bytes,
        installed_bytes,
        assets: states,
    })
}

fn install(args: &InstallArgs) -> Result<()> {
    let (source, license, assets) = assets(&args.model)?;
    let root = models_root(args.home.clone())?.join(&args.model);
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .user_agent(concat!("ato-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build download client")?;
    let outcome = install_assets(&client, &assets, &root, args.json)?;
    let total_bytes = assets.iter().map(|asset| asset.bytes).sum::<u64>();
    let report = InstallReport {
        schema: INSTALL_SCHEMA,
        model: args.model.clone(),
        root: root.display().to_string(),
        license,
        source,
        downloaded_bytes: outcome.downloaded_bytes,
        total_bytes,
        already_present: outcome.downloaded_bytes == 0,
    };
    write_provenance(&root, &report, &assets)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "{}: {} of {} bytes downloaded into {}",
            report.model, report.downloaded_bytes, report.total_bytes, report.root
        );
        println!(
            "license: {} — see the upstream repositories for the terms",
            report.license
        );
    }
    Ok(())
}

fn remove(args: &RemoveArgs) -> Result<()> {
    let root = models_root(args.home.clone())?.join(&args.model);
    let mut removed_bytes = 0u64;
    let mut removed = false;
    if root.is_dir() {
        removed_bytes = directory_bytes(&root)?;
        fs::remove_dir_all(&root).with_context(|| format!("remove {}", root.display()))?;
        removed = true;
    }
    // Reject an unknown Model Set name even though nothing was installed.
    assets(&args.model)?;
    let report = RemoveReport {
        schema: INSTALL_SCHEMA,
        model: args.model.clone(),
        root: root.display().to_string(),
        removed_bytes,
        removed,
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if removed {
        println!("removed {} ({removed_bytes} bytes)", report.root);
    } else {
        println!("nothing installed for {}", report.model);
    }
    Ok(())
}

#[derive(Debug)]
struct InstallOutcome {
    downloaded_bytes: u64,
}

fn install_assets(
    client: &reqwest::blocking::Client,
    assets: &[Asset],
    root: &Path,
    quiet: bool,
) -> Result<InstallOutcome> {
    let mut downloaded_bytes = 0u64;
    for asset in assets {
        let target = root.join(asset.path);
        if verify_asset(&target, asset) {
            if !quiet {
                eprintln!("verified {}", asset.path);
            }
            continue;
        }
        downloaded_bytes += fetch_asset(client, asset, root, quiet)?;
    }
    Ok(InstallOutcome { downloaded_bytes })
}

/// True when the target already holds exactly the pinned payload.
fn verify_asset(target: &Path, asset: &Asset) -> bool {
    match fs::metadata(target) {
        Ok(metadata) if metadata.is_file() && metadata.len() == asset.bytes => {
            matches!(file_digest(target), Ok(digest) if digest == asset.sha256)
        }
        _ => false,
    }
}

fn fetch_asset(
    client: &reqwest::blocking::Client,
    asset: &Asset,
    root: &Path,
    quiet: bool,
) -> Result<u64> {
    let target = root.join(asset.path);
    let parent = target
        .parent()
        .with_context(|| format!("{} has no parent directory", asset.path))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("{} has no file name", asset.path))?;
    let partial = parent.join(format!(".{name}.partial"));
    let mut resume_from = match fs::metadata(&partial) {
        Ok(metadata) if metadata.is_file() => metadata.len(),
        _ => 0,
    };
    if resume_from >= asset.bytes {
        fs::remove_file(&partial).ok();
        resume_from = 0;
    }
    if !quiet && resume_from > 0 {
        eprintln!("resuming {} at {resume_from} bytes", asset.path);
    } else if !quiet {
        eprintln!("downloading {} ({} bytes)", asset.path, asset.bytes);
    }
    let mut request = client.get(&asset.url);
    if resume_from > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={resume_from}-"));
    }
    let mut response = request
        .send()
        .with_context(|| format!("request {}", asset.url))?;
    let status = response.status();
    let append = if resume_from > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT {
        true
    } else if status == reqwest::StatusCode::OK {
        resume_from = 0;
        false
    } else {
        bail!(
            "model_download_failed: {} returned HTTP {} from {}{}",
            asset.path,
            status.as_u16(),
            asset.url,
            response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .map(|value| format!(" (location: {value})"))
                .unwrap_or_default()
        );
    };
    let mut hasher = Sha256::new();
    if append {
        let mut existing = File::open(&partial).context("reopen partial download")?;
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = existing.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
    }
    let file = if append {
        fs::OpenOptions::new().append(true).open(&partial)?
    } else {
        File::create(&partial)?
    };
    let mut writer = BufWriter::new(file);
    let mut written = resume_from;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = response.read(&mut buffer).context("read download body")?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        writer
            .write_all(&buffer[..read])
            .context("write download body")?;
        written += read as u64;
        ensure!(
            written <= asset.bytes,
            "model_download_oversized: {} exceeded its pinned {bytes} bytes",
            asset.path,
            bytes = asset.bytes
        );
    }
    writer.flush()?;
    drop(writer);
    let digest = format!("{:x}", hasher.finalize());
    if written != asset.bytes || digest != asset.sha256 {
        fs::remove_file(&partial).ok();
        bail!(
            "model_asset_digest_mismatch: {} produced {written} bytes / sha256:{digest}, expected {bytes} bytes / sha256:{expected}",
            asset.path,
            bytes = asset.bytes,
            expected = asset.sha256
        );
    }
    fs::rename(&partial, &target).with_context(|| format!("publish {}", target.display()))?;
    Ok(asset.bytes)
}

fn write_provenance(root: &Path, report: &InstallReport, assets: &[Asset]) -> Result<()> {
    #[derive(Serialize)]
    struct Provenance<'a> {
        schema: &'static str,
        model: &'a str,
        installed_at_unix: u64,
        license: &'static str,
        source: &'a ModelSource,
        total_bytes: u64,
        assets: Vec<InstalledAsset>,
    }
    let record = Provenance {
        schema: INSTALL_SCHEMA,
        model: &report.model,
        installed_at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        license: report.license,
        source: &report.source,
        total_bytes: report.total_bytes,
        assets: assets
            .iter()
            .map(|asset| InstalledAsset {
                path: asset.path.to_owned(),
                role: asset.role.to_owned(),
                bytes: asset.bytes,
                sha256: asset.sha256.to_owned(),
            })
            .collect(),
    };
    let path = root.join("provenance.json");
    let bytes = serde_json::to_vec_pretty(&record)?;
    fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn file_digest(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn directory_bytes(root: &Path) -> Result<u64> {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in
            fs::read_dir(&directory).with_context(|| format!("read {}", directory.display()))?
        {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if metadata.is_file() {
                total += metadata.len();
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    /// The pinned table is the only thing standing between a user and unverified
    /// model bytes, so it is checked as data.
    /// The runner protocol is the only contract between the CLI and the model, so
    /// it is exercised with a stand-in interpreter: no model, no MLX, no network.
    fn stub_runner(body: &str) -> (PathBuf, PathBuf) {
        let root = temp_root();
        let script = root.join("stub.py");
        fs::write(&script, body).expect("write stub");
        (root, script)
    }

    #[test]
    fn a_runner_answer_is_parsed_into_the_score_report() {
        let (root, script) = stub_runner(
            "import json,sys\n\
             request = json.load(sys.stdin)\n\
             assert request['model_root'], 'the runner needs the verified root'\n\
             json.dump({'answers': {'team': {'choice': 'billing'}}, 'raw': [[0.9, 0.1]],\n\
                        'runtime': {'backend': 'mlx', 'device': 'mps', 'python': '3.12.4',\n\
                                    'load_seconds': 1.5, 'inference_ms': 12.0},\n\
                        'checkpoint': {'weights': 'lora', 'base': 'Qwen/Qwen3.5-0.8B-Base',\n\
                                       'head_dim': 256, 'temperature': 2.35}}, sys.stdout)\n",
        );
        let value = invoke_runner(
            Path::new("python3"),
            &script,
            &serde_json::json!({"state": "s", "questions": {"team": {}}, "model_root": "/m"}),
        )
        .expect("runner answer");
        assert_eq!(value["answers"]["team"]["choice"], "billing");
        assert_eq!(value["runtime"]["backend"], "mlx");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_runner_error_is_reported_with_its_own_code() {
        let (root, script) = stub_runner(
            "import json,sys\n\
             json.dump({'error': {'code': 'model_set_incomplete', 'message': 'head.pt is missing'}}, sys.stdout)\n",
        );
        let error = invoke_runner(
            Path::new("python3"),
            &script,
            &serde_json::json!({"state": "s", "questions": {}, "model_root": "/m"}),
        )
        .expect_err("a runner error must fail");
        assert!(
            error.to_string().contains("model_set_incomplete"),
            "unexpected error: {error}"
        );
        assert!(error.to_string().contains("head.pt is missing"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_runner_that_prints_nothing_is_a_runtime_failure_not_a_silent_success() {
        let (root, script) = stub_runner("import sys\nsys.stderr.write('boom')\nsys.exit(3)\n");
        let error = invoke_runner(
            Path::new("python3"),
            &script,
            &serde_json::json!({"state": "s", "questions": {}, "model_root": "/m"}),
        )
        .expect_err("a silent runner must fail");
        assert!(
            error.to_string().contains("model_runtime_failed"),
            "unexpected error: {error}"
        );
        assert!(
            error.to_string().contains("boom"),
            "stderr must be surfaced"
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_request_without_questions_is_refused_before_the_model_starts() {
        let error =
            read_request("{\"state\": \"s\", \"questions\": {}}").expect_err("no questions");
        assert!(error.to_string().contains("at least one question"));
        let error = read_request("{\"questions\": {\"a\": {}}}").expect_err("no state");
        assert!(error.to_string().contains("needs a state"));
        let error = read_request("not json").expect_err("not json");
        assert!(error.to_string().contains("not valid JSON"));
        let request = read_request("{\"state\": \"s\", \"questions\": {\"a\": {}}}").expect("ok");
        assert_eq!(request.state, serde_json::json!("s"));
    }

    #[test]
    fn the_embedded_runner_is_the_shipped_script() {
        assert!(KEV_RUNNER.contains("model_root"));
        assert!(KEV_RUNNER.contains("Qwen/Qwen3.5-0.8B-Base"));
        // The runner must never reach the network for a pinned payload.
        assert!(!KEV_RUNNER.contains("snapshot_download"));
        assert!(!KEV_RUNNER.contains("from_pretrained(\"Qwen"));
    }

    #[test]
    fn the_pinned_asset_table_is_well_formed() {
        let (source, license, assets) = assets(KEV_SET).expect("kev manifest");
        assert_eq!(license, "Apache-2.0");
        assert_eq!(source.revision.len(), 40);
        assert_eq!(
            source.base_revision.as_deref().map(str::len),
            Some(40),
            "the base model must be pinned to a revision"
        );
        let mut seen = std::collections::BTreeSet::new();
        for asset in &assets {
            assert!(seen.insert(asset.path), "duplicate asset {}", asset.path);
            assert_eq!(asset.sha256.len(), 64, "{} digest length", asset.path);
            assert!(
                asset
                    .sha256
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{} digest must be lowercase hex",
                asset.path
            );
            assert!(asset.bytes > 0, "{} byte count", asset.path);
            assert!(
                asset.url.starts_with("https://huggingface.co/"),
                "{} must resolve from the pinned upstream",
                asset.path
            );
            assert!(
                asset
                    .url
                    .ends_with(&format!("/{}", upstream_path(asset.path))),
                "{} must download its declared upstream name",
                asset.path
            );
            let pinned_revision = asset.url.contains(source.revision.as_str())
                || asset
                    .url
                    .contains(source.base_revision.as_deref().expect("base revision"));
            assert!(
                pinned_revision,
                "{} must resolve from a pinned revision",
                asset.path
            );
        }
        assert!(assets.iter().any(|asset| asset.path == "head.pt"));
        let total: u64 = assets.iter().map(|asset| asset.bytes).sum();
        assert!(
            (1_700_000_000..1_900_000_000).contains(&total),
            "unexpected total payload {total}"
        );
        assert!(super::assets("no-such-model").is_err());
    }

    /// A single-file HTTP origin standing in for the upstream host, so resume,
    /// digest verification and failure handling are exercised without network.
    struct Origin {
        port: u16,
        hits: Arc<AtomicUsize>,
    }

    fn origin(bytes: Vec<u8>) -> Origin {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let hits = Arc::new(AtomicUsize::new(0));
        let served = bytes.clone();
        let counter = Arc::clone(&hits);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() {
                    continue;
                }
                let mut range = None;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                        break;
                    }
                    if let Some(value) = header
                        .to_ascii_lowercase()
                        .strip_prefix("range: bytes=")
                        .map(str::to_owned)
                    {
                        range = value
                            .split('-')
                            .next()
                            .and_then(|n| n.parse::<usize>().ok());
                    }
                }
                let start = range.unwrap_or(0).min(served.len());
                let body = &served[start..];
                let head = if start > 0 {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                        body.len(),
                        start,
                        served.len().saturating_sub(1),
                        served.len()
                    )
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                };
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body);
                let _ = stream.flush();
            }
        });
        Origin { port, hits }
    }

    fn temp_root() -> PathBuf {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".tmp/model-install");
        fs::create_dir_all(&base).expect("test root");
        let root = base.join(format!(
            "ato-model-install-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("temp root");
        root
    }

    fn asset_for(port: u16, payload: &[u8]) -> Asset {
        let digest = {
            let mut hasher = Sha256::new();
            hasher.update(payload);
            format!("{:x}", hasher.finalize())
        };
        Asset {
            path: "base/payload.bin",
            role: "base",
            url: format!("http://127.0.0.1:{port}/base/payload.bin"),
            sha256: Box::leak(digest.into_boxed_str()),
            bytes: payload.len() as u64,
        }
    }

    #[test]
    fn an_asset_is_downloaded_verified_and_published_atomically() {
        let payload = b"kev pointer head placeholder payload".to_vec();
        let origin = origin(payload.clone());
        let asset = asset_for(origin.port, &payload);
        let root = temp_root();
        let client = reqwest::blocking::Client::new();
        let outcome =
            install_assets(&client, std::slice::from_ref(&asset), &root, true).expect("install");
        assert_eq!(outcome.downloaded_bytes, payload.len() as u64);
        let installed = root.join(asset.path);
        assert_eq!(fs::read(&installed).expect("read back"), payload);
        assert!(!root.join("base/.payload.bin.partial").exists());
        // A second run must not touch the network at all.
        let hits_before = origin.hits.load(Ordering::SeqCst);
        let outcome =
            install_assets(&client, std::slice::from_ref(&asset), &root, true).expect("reinstall");
        assert_eq!(outcome.downloaded_bytes, 0);
        assert_eq!(origin.hits.load(Ordering::SeqCst), hits_before);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_resumed_download_continues_from_the_partial_file() {
        let payload: Vec<u8> = (0..64 * 1024).map(|index| (index % 251) as u8).collect();
        let origin = origin(payload.clone());
        let asset = asset_for(origin.port, &payload);
        let root = temp_root();
        let partial_dir = root.join("base");
        fs::create_dir_all(&partial_dir).expect("partial dir");
        fs::write(
            partial_dir.join(".payload.bin.partial"),
            &payload[..32 * 1024],
        )
        .expect("seed partial");
        let client = reqwest::blocking::Client::new();
        install_assets(&client, std::slice::from_ref(&asset), &root, true).expect("resume");
        assert_eq!(fs::read(root.join(asset.path)).expect("read"), payload);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_payload_that_does_not_match_the_pin_is_rejected_and_not_kept() {
        let served = b"tampered payload".to_vec();
        let pinned = b"the pinned payload".to_vec();
        let origin = origin(served);
        let asset = asset_for(origin.port, &pinned);
        let root = temp_root();
        let client = reqwest::blocking::Client::new();
        let error = install_assets(&client, std::slice::from_ref(&asset), &root, true)
            .expect_err("digest mismatch must fail");
        assert!(
            error.to_string().contains("model_asset_digest_mismatch"),
            "unexpected error: {error}"
        );
        assert!(!root.join(asset.path).exists());
        assert!(!root.join("base/.payload.bin.partial").exists());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn status_reports_missing_then_installed_assets_and_verifies_on_request() {
        let home = temp_root();
        let report = status_report(KEV_SET, Some(home.clone()), false).expect("status");
        assert!(!report.installed);
        assert_eq!(report.installed_bytes, 0);
        assert!(report.assets.iter().all(|asset| !asset.present));
        assert_eq!(report.schema, "ato.model-status/1");
        // A payload of the right size but wrong bytes is present until a digest
        // check is requested.
        let root = home.join("models").join(KEV_SET).join("base");
        fs::create_dir_all(&root).expect("mkdir");
        let first = assets(KEV_SET).expect("manifest").2[0].clone();
        fs::write(root.join("config.json"), vec![0u8; first.bytes as usize]).expect("write");
        let report = status_report(KEV_SET, Some(home.clone()), false).expect("status");
        assert!(!report.installed);
        assert!(report.assets[0].present);
        assert_eq!(report.assets[0].verified, None);
        let report = status_report(KEV_SET, Some(home.clone()), true).expect("verify");
        assert!(!report.assets[0].present, "wrong bytes must not verify");
        assert_eq!(report.assets[0].verified, Some(false));
        fs::remove_dir_all(home).ok();
    }
}
