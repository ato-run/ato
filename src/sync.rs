//! Sync runtime module for .sync format support
//!
//! This module provides functionality to:
//! - Open and mount .sync files
//! - Access payload with zero-copy
//! - Atomic payload updates
//! - Execute sync.wasm in strict sandbox

use capsule_sync::{Result as SyncResult, SyncArchive, VfsMount, VfsMountConfig};
use std::fs::File;
use std::io::{self, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{interval, Duration};

pub use capsule_sync::{NetworkScope, SharePolicy};

pub struct SyncRuntime {
    archive: SyncArchive,
    mount: VfsMount,
    sync_path: Option<PathBuf>,
    network_scope: NetworkScope,
}

impl SyncRuntime {
    pub fn open<P: AsRef<std::path::Path>>(path: P) -> SyncResult<Self> {
        let path = path.as_ref();
        let archive = SyncArchive::open(path)?;
        let config = VfsMountConfig::default();
        let mount = VfsMount::from_archive(&archive, config)?;

        Ok(Self {
            archive,
            mount,
            sync_path: Some(path.to_path_buf()),
            network_scope: NetworkScope::Local,
        })
    }

    pub fn with_network_scope<P: AsRef<std::path::Path>>(
        path: P,
        scope: NetworkScope,
    ) -> SyncResult<Self> {
        let path = path.as_ref();
        let archive = SyncArchive::open(path)?;
        let config = VfsMountConfig::default();
        let mount = VfsMount::from_archive(&archive, config)?;

        Ok(Self {
            archive,
            mount,
            sync_path: Some(path.to_path_buf()),
            network_scope: scope,
        })
    }

    pub fn archive(&self) -> &SyncArchive {
        &self.archive
    }

    pub fn mount(&self) -> &VfsMount {
        &self.mount
    }

    pub fn network_scope(&self) -> NetworkScope {
        self.network_scope
    }

    pub fn set_network_scope(&mut self, scope: NetworkScope) {
        self.network_scope = scope;
    }

    pub fn is_expired(&self) -> anyhow::Result<bool> {
        self.archive
            .manifest()
            .is_expired()
            .map_err(|e| anyhow::anyhow!(e))
    }

    pub fn should_auto_update(&self) -> anyhow::Result<bool> {
        Ok(self.is_expired()?)
    }

    pub fn update_payload(&mut self, new_payload: &[u8]) -> SyncResult<()> {
        self.archive.update_payload(new_payload)
    }

    pub fn execute_wasm(&mut self) -> anyhow::Result<Vec<u8>> {
        if !self.archive.has_wasm() {
            return Err(anyhow::anyhow!("No sync.wasm found in archive"));
        }

        let sync_path = self
            .sync_path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Sync path not set"))?;

        let file = File::open(sync_path)?;
        let mut archive = zip::ZipArchive::new(file)?;

        let mut wasm_file = archive.by_name("sync.wasm")?;
        let mut wasm_bytes = Vec::new();
        wasm_file.read_to_end(&mut wasm_bytes)?;

        let context = if self.archive.has_context() {
            let file = File::open(sync_path)?;
            let mut archive = zip::ZipArchive::new(file)?;
            let mut context_file = archive.by_name("context.json")?;
            let mut context_data = Vec::new();
            context_file.read_to_end(&mut context_data)?;
            String::from_utf8(context_data)?
        } else {
            String::new()
        };

        let payload_entry = self
            .mount
            .get_payload_entry()
            .ok_or_else(|| anyhow::anyhow!("Payload entry not found"))?;

        let payload_offset = payload_entry.offset;
        let payload_size = payload_entry.size;

        let mut sync_file = File::open(sync_path)?;
        sync_file.seek(io::SeekFrom::Start(payload_offset))?;
        let mut payload_bytes = vec![0u8; payload_size as usize];
        sync_file.read_exact(&mut payload_bytes)?;

        let sandbox = WasmSandbox::new()?;
        let permissions = self.archive.manifest().permissions.clone();
        let timeout = self.archive.manifest().policy.timeout;

        let new_payload =
            sandbox.execute(&wasm_bytes, &context, &payload_bytes, &permissions, timeout)?;

        Ok(new_payload)
    }

    pub fn execute_and_update(&mut self) -> anyhow::Result<()> {
        let new_payload = self.execute_wasm()?;
        self.update_payload(&new_payload)?;
        Ok(())
    }

    pub fn auto_update_if_expired(&mut self) -> anyhow::Result<bool> {
        if !self.should_auto_update()? {
            return Ok(false);
        }

        self.execute_and_update()?;
        Ok(true)
    }

    pub fn spawn_auto_update_task(
        runtime: Arc<Mutex<Self>>,
        tick_interval: Duration,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = interval(tick_interval);
            loop {
                ticker.tick().await;
                if let Ok(mut guard) = runtime.try_lock() {
                    let _ = guard.auto_update_if_expired();
                }
            }
        })
    }

    pub fn spawn_ttl_scheduler(runtime: Arc<Mutex<Self>>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let sleep_for = {
                    let guard = runtime.lock().await;
                    guard
                        .next_ttl_interval()
                        .unwrap_or_else(|_| Duration::from_secs(60))
                };

                tokio::time::sleep(sleep_for).await;

                if let Ok(mut guard) = runtime.try_lock() {
                    let _ = guard.auto_update_if_expired();
                }
            }
        })
    }

    fn next_ttl_interval(&self) -> anyhow::Result<Duration> {
        let duration = self
            .archive
            .manifest()
            .expires_in()
            .map_err(|e| anyhow::anyhow!(e))?;
        let secs = duration.num_seconds();
        let secs = if secs <= 0 { 1 } else { secs } as u64;
        Ok(Duration::from_secs(secs))
    }
}

use tempfile::TempDir;
use wasi_common::pipe::{ReadPipe, WritePipe};
use wasmtime::{Config, Engine, Linker, Module, Store};
use wasmtime_wasi::sync::{ambient_authority, Dir, WasiCtxBuilder};

pub struct WasmSandbox {
    engine: Engine,
}

impl WasmSandbox {
    pub fn new() -> anyhow::Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(false);
        config.async_support(false);
        config.max_wasm_stack(1024 * 1024);

        let engine = Engine::new(&config)?;

        Ok(Self { engine })
    }

    pub fn execute(
        &self,
        wasm_bytes: &[u8],
        context: &str,
        payload: &[u8],
        permissions: &capsule_sync::ManifestPermissions,
        _timeout_secs: u64,
    ) -> anyhow::Result<Vec<u8>> {
        let module = Module::from_binary(&self.engine, wasm_bytes)?;

        let temp_dir = TempDir::new()?;
        let payload_path = temp_dir.path().join("payload");
        let context_path = temp_dir.path().join("context.json");

        std::fs::write(&payload_path, payload)?;
        std::fs::write(&context_path, context)?;

        set_readonly(&payload_path)?;
        set_readonly(&context_path)?;

        let mut wasi_builder = WasiCtxBuilder::new();

        let stdout_pipe = WritePipe::new_in_memory();
        let stderr_pipe = WritePipe::new_in_memory();
        let stdout_handle = stdout_pipe.clone();

        wasi_builder.arg("sync.wasm")?;
        wasi_builder.env("SYNC_PATH", "/sync")?;
        wasi_builder.env("SYNC_PAYLOAD", "/sync/payload")?;
        wasi_builder.env("SYNC_CONTEXT", "/sync/context.json")?;
        wasi_builder
            .stdin(Box::new(ReadPipe::from(Vec::new())))
            .stdout(Box::new(stdout_pipe))
            .stderr(Box::new(stderr_pipe));

        for env_var in &permissions.allow_env {
            if let Ok(value) = std::env::var(env_var) {
                let _ = wasi_builder.env(env_var, &value);
            }
        }

        let allow_hosts = permissions.allow_hosts.join(",");
        let _ = wasi_builder.env("ALLOW_HOSTS", &allow_hosts);

        let dir = Dir::open_ambient_dir(temp_dir.path(), ambient_authority())?;
        let _ = wasi_builder.preopened_dir(dir, "/sync");

        let wasi = wasi_builder.build();

        let mut linker = Linker::new(&self.engine);
        wasmtime_wasi::add_to_linker(&mut linker, |ctx| ctx)?;

        let mut store = Store::new(&self.engine, wasi);

        let instance = linker.instantiate(&mut store, &module)?;

        let start = instance.get_typed_func::<(), ()>(&mut store, "_start")?;

        let _result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| start.call(&mut store, ())));

        let output = stdout_handle
            .try_into_inner()
            .map_err(|_| anyhow::anyhow!("stdout handle still in use"))?
            .into_inner();

        Ok(output)
    }
}

#[cfg(unix)]
fn set_readonly(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let permissions = std::fs::Permissions::from_mode(0o444);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_readonly(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}
