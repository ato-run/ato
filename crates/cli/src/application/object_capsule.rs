//! Product workflow for importing and advancing portable computation bundles.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use ato_adapter_repository::materialize_source;
use ato_computation::{ContentRef, SemanticsId};
use ato_kernel::{Action, Kernel, Run};
use ato_objects::{
    MemoryObjectStore, ObjectResolver, ReferenceRegistry, decode_bundle, import_bundle,
    read_exact_object,
};
use ato_semantics_compose::ComposeReferences;
use ato_semantics_workspace::{
    MAX_WORKSPACE_RESIDUAL_BYTES, WORKSPACE_SEMANTICS_ID, WorkspaceReferences, WorkspaceSemantics,
    decode_workspace_residual, observe_exit,
};
use nacelle::workspace_provider::NacelleWorkspaceProvider;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunRecord {
    name: String,
    head: String,
    bundle: PathBuf,
    pid: u32,
    status: String,
    exit_code: Option<i32>,
    log: PathBuf,
}

pub(crate) fn start(bundle: &Path, name: Option<&str>, detach: bool, worker: bool) -> Result<()> {
    let bundle = bundle
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", bundle.display()))?;
    let name = safe_name(name.or_else(|| bundle.file_stem().and_then(|value| value.to_str())))?;
    if detach && !worker {
        let log = record_dir(&name)?.join("output.log");
        fs::create_dir_all(log.parent().expect("log parent"))?;
        let stdout = OpenOptions::new().create(true).append(true).open(&log)?;
        let stderr = stdout.try_clone()?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["decap", "start"])
            .arg(&bundle)
            .args(["--name", &name, "--worker"])
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        let child = command
            .spawn()
            .context("failed to start detached computation run")?;
        println!("started {name} (pid {})", child.id());
        return Ok(());
    }
    advance_bundle(&bundle, &name)
}

fn advance_bundle(bundle_path: &Path, name: &str) -> Result<()> {
    let bytes = fs::read(bundle_path)?;
    let bundle = decode_bundle(&bytes).context("invalid .capsule object bundle")?;
    let objects = Arc::new(MemoryObjectStore::default());
    let mut references = ReferenceRegistry::default();
    references.register(Arc::new(WorkspaceReferences::default()))?;
    references.register(Arc::new(ComposeReferences::default()))?;
    let root = import_bundle(&bundle, objects.as_ref(), &references)?;
    let resolved = ato_objects::resolve_computation(objects.as_ref(), &root)?;
    let workspace_id = SemanticsId::parse(WORKSPACE_SEMANTICS_ID).expect("static semantics id");
    if resolved.object().semantics != workspace_id {
        bail!(
            "root computation uses {}; direct decap execution currently requires {}",
            resolved.object().semantics,
            WORKSPACE_SEMANTICS_ID
        );
    }
    let residual_ref = &resolved.object().residual;
    let metadata = objects.metadata(residual_ref)?;
    let residual = decode_workspace_residual(&read_exact_object(
        objects.as_ref(),
        residual_ref,
        metadata.size,
        MAX_WORKSPACE_RESIDUAL_BYTES,
    )?)?;
    let source = ContentRef::parse(&residual.source)?;
    let directory = record_dir(name)?;
    fs::create_dir_all(&directory)?;
    let workspace = directory.join("workspace");
    if workspace.exists() {
        fs::remove_dir_all(&workspace)?;
    }
    materialize_source(&source, objects.as_ref(), &workspace)?;
    let log = directory.join("output.log");
    let record_path = directory.join("run.json");
    let mut record = RunRecord {
        name: name.to_owned(),
        head: root.to_string(),
        bundle: bundle_path.to_path_buf(),
        pid: std::process::id(),
        status: "running".to_owned(),
        exit_code: None,
        log,
    };
    write_record(&record_path, &record)?;

    let provider = Arc::new(NacelleWorkspaceProvider::default());
    provider.bind_source(source, &workspace)?;
    let mut kernel = Kernel::<()>::new(objects.clone());
    kernel.register(Arc::new(WorkspaceSemantics::new(provider)))?;
    let mut run = Run { head: root };
    let outcome = kernel.step(&mut run, &Action::Tau);
    record.head = run.head.to_string();
    match outcome {
        Ok(_) => {
            let resolved = kernel.resolve(&run.head)?;
            let metadata = objects.metadata(&resolved.object().residual)?;
            let residual = decode_workspace_residual(&read_exact_object(
                objects.as_ref(),
                &resolved.object().residual,
                metadata.size,
                MAX_WORKSPACE_RESIDUAL_BYTES,
            )?)?;
            let exit = observe_exit(&residual).context("workspace did not expose an exit")?;
            record.status = "exited".to_owned();
            record.exit_code = Some(exit);
            write_record(&record_path, &record)?;
            if exit == 0 {
                Ok(())
            } else {
                bail!("computation exited with status {exit}")
            }
        }
        Err(error) => {
            record.status = "failed".to_owned();
            write_record(&record_path, &record)?;
            Err(error.into())
        }
    }
}

pub(crate) fn list(json: bool) -> Result<()> {
    let root = runs_root()?;
    let mut records = Vec::new();
    if root.is_dir() {
        for entry in fs::read_dir(root)? {
            let path = entry?.path().join("run.json");
            if let Ok(bytes) = fs::read(path)
                && let Ok(record) = serde_json::from_slice::<RunRecord>(&bytes)
            {
                records.push(record);
            }
        }
    }
    records.sort_by(|left, right| left.name.cmp(&right.name));
    if json {
        println!("{}", serde_json::to_string(&records)?);
    } else {
        for record in records {
            println!("{}\t{}\t{}", record.name, record.status, record.head);
        }
    }
    Ok(())
}

pub(crate) fn attach(name: &str) -> Result<()> {
    let record = read_record(name)?;
    if record.log.is_file() {
        print!("{}", fs::read_to_string(record.log)?);
    }
    Ok(())
}

pub(crate) fn stop(name: &str) -> Result<()> {
    let mut record = read_record(name)?;
    if record.status != "running" {
        return Ok(());
    }
    #[cfg(unix)]
    unsafe {
        if libc::kill(record.pid as i32, libc::SIGTERM) != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    #[cfg(not(unix))]
    bail!("stopping detached computation runs is unavailable on this platform");
    record.status = "stopped".to_owned();
    write_record(&record_dir(name)?.join("run.json"), &record)
}

fn runs_root() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("home directory is unavailable")?
        .join(".ato/runs/computations"))
}

fn record_dir(name: &str) -> Result<PathBuf> {
    Ok(runs_root()?.join(safe_name(Some(name))?))
}

fn safe_name(name: Option<&str>) -> Result<String> {
    let name = name.unwrap_or("capsule");
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("run name must contain only ASCII letters, digits, '-' or '_'");
    }
    Ok(name.to_owned())
}

fn write_record(path: &Path, record: &RunRecord) -> Result<()> {
    let parent = path.parent().context("record path has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension("json.new");
    fs::write(&temporary, serde_json::to_vec_pretty(record)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn read_record(name: &str) -> Result<RunRecord> {
    let path = record_dir(name)?.join("run.json");
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
