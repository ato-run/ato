//! One owned Docker main-process PTY, behind the existing private Terminal wire.
use anyhow::{Context, Result, bail, ensure};
use ato_adapter_oci::OciTerminal;
use ato_ipc::terminal_surface::{
    MAX_UNACKED_TERMINAL_OUTPUT_BYTES, TerminalClientControl, TerminalServerControl,
};
use netd::terminal_gateway::{
    FRAME_CONTROL, FRAME_INPUT, FRAME_OUTPUT, read_guest_frame, write_guest_frame,
};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::mpsc,
    task::JoinHandle,
    time::timeout,
};

type Output = Result<Vec<u8>>;
struct OwnedSocket(PathBuf);
impl Drop for OwnedSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
pub(super) struct TerminalProducer {
    terminal: Arc<Mutex<OciTerminal>>,
    task: JoinHandle<Result<()>>,
    readers: Vec<std::thread::JoinHandle<()>>,
    path: OwnedSocket,
    failed: Arc<AtomicBool>,
}
impl TerminalProducer {
    pub async fn start(
        mut terminal: OciTerminal,
        path: PathBuf,
        cols: u16,
        rows: u16,
    ) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let listener = UnixListener::bind(&path).context("bind owned terminal socket")?;
        let path = OwnedSocket(path);
        std::fs::set_permissions(&path.0, std::fs::Permissions::from_mode(0o600))?;
        terminal.resize(cols, rows)?;
        let (input, output, errors) = terminal.take_io()?;
        let terminal = Arc::new(Mutex::new(terminal));
        let (tx, rx) = mpsc::channel::<Output>(8);
        let mut readers = Vec::new();
        let failed = Arc::new(AtomicBool::new(false));
        for (is_error, mut pipe) in [
            (false, Box::new(output) as Box<dyn Read + Send>),
            (true, Box::new(errors) as Box<dyn Read + Send>),
        ] {
            let tx = tx.clone();
            let failed = Arc::clone(&failed);
            readers.push(std::thread::spawn(move || {
                let mut bytes = [0_u8; 16 * 1024];
                loop {
                    match pipe.read(&mut bytes) {
                        Ok(0) => {
                            if !is_error {
                                failed.store(true, Ordering::Release);
                                let _ = tx.blocking_send(Err(anyhow::anyhow!(
                                    "owned terminal output ended"
                                )));
                            }
                            break;
                        }
                        Ok(_) if is_error => {
                            failed.store(true, Ordering::Release);
                            let _ = tx.blocking_send(Err(anyhow::anyhow!(
                                "owned terminal attach failed"
                            )));
                            break;
                        }
                        Ok(n) => {
                            if tx.blocking_send(Ok(bytes[..n].to_vec())).is_err() {
                                break;
                            }
                        }
                        Err(_) => {
                            failed.store(true, Ordering::Release);
                            let _ = tx
                                .blocking_send(Err(anyhow::anyhow!("owned terminal pipe failed")));
                            break;
                        }
                    }
                }
            }));
        }
        drop(tx);
        let owned = Arc::clone(&terminal);
        let task = tokio::spawn(serve(
            listener,
            rx,
            Arc::new(Mutex::new(input)),
            owned,
            cols,
            rows,
        ));
        Ok(Self {
            terminal,
            task,
            readers,
            path,
            failed,
        })
    }
    pub fn failed(&self) -> bool {
        self.task.is_finished() || self.failed.load(Ordering::Acquire)
    }
    pub async fn stop(&mut self) -> Result<()> {
        self.task.abort();
        let _ = (&mut self.task).await;
        let _ = std::fs::remove_file(&self.path.0);
        self.terminal
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal owner poisoned"))?
            .stop()?;
        for reader in self.readers.drain(..) {
            tokio::task::spawn_blocking(move || reader.join())
                .await?
                .map_err(|_| anyhow::anyhow!("terminal pipe reader panicked"))?;
        }
        Ok(())
    }
}
impl Drop for TerminalProducer {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path.0);
        if let Ok(mut terminal) = self.terminal.lock() {
            let _ = terminal.stop();
        }
    }
}

async fn serve(
    listener: UnixListener,
    mut output: mpsc::Receiver<Output>,
    input: Arc<Mutex<std::process::ChildStdin>>,
    terminal: Arc<Mutex<OciTerminal>>,
    mut cols: u16,
    mut rows: u16,
) -> Result<()> {
    let mut pending = VecDeque::new();
    let mut pending_bytes = 0;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let result = relay(stream, &mut output, &input, &terminal, &mut pending, &mut pending_bytes, &mut cols, &mut rows).await;
                if let Err(error) = result {
                    // A disconnected viewer may reconnect; producer/pipe failures
                    // are checked independently before another connection is served.
                    if output.is_closed() { return Err(error); }
                }
            }
            bytes = output.recv() => {
                let bytes = bytes.context("owned terminal output closed")??;
                pending_bytes += bytes.len();
                ensure!(pending_bytes <= MAX_UNACKED_TERMINAL_OUTPUT_BYTES, "unobserved terminal output exceeded bound");
                pending.push_back(bytes);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn relay(
    mut stream: UnixStream,
    output: &mut mpsc::Receiver<Output>,
    input: &Arc<Mutex<std::process::ChildStdin>>,
    terminal: &Arc<Mutex<OciTerminal>>,
    pending: &mut VecDeque<Vec<u8>>,
    pending_bytes: &mut usize,
    cols: &mut u16,
    rows: &mut u16,
) -> Result<()> {
    timeout(Duration::from_secs(1), async {
        let mut line = Vec::new();
        while line.len() <= 32 {
            let byte = stream.read_u8().await?;
            line.push(byte);
            if byte == b'\n' {
                break;
            }
        }
        ensure!(
            line == b"CONNECT 1026\n",
            "invalid private terminal handshake"
        );
        stream.write_all(b"OK 1026\n").await?;
        write_guest_frame(
            &mut stream,
            FRAME_CONTROL,
            &serde_json::to_vec(&TerminalServerControl::Ready {
                cols: *cols,
                rows: *rows,
            })?,
        )
        .await?;
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    let (mut read, mut write) = stream.into_split();
    let (tx, mut frames) = mpsc::channel(8);
    let mut readers = tokio::task::JoinSet::new();
    readers.spawn(async move {
        loop {
            let frame = read_guest_frame(&mut read).await;
            let ended = frame.is_err();
            if tx.send(frame).await.is_err() || ended {
                break;
            }
        }
    });
    // Keep partial frame reads alive when output arrives; read_exact is not
    // cancellation-safe. Aborting this connection always closes its reader.
    let result = async {
        loop {
            if let Some(bytes) = pending.pop_front() {
                *pending_bytes -= bytes.len();
                timeout(Duration::from_secs(2), write_guest_frame(&mut write, FRAME_OUTPUT, &bytes)).await??;
                continue;
            }
            tokio::select! {
                bytes = output.recv() => {
                    let bytes = bytes.context("owned terminal output closed")??;
                    timeout(Duration::from_secs(2), write_guest_frame(&mut write, FRAME_OUTPUT, &bytes)).await??;
                }
                frame = frames.recv() => {
                    let (kind, payload) = frame.context("terminal viewer closed")??;
                    match kind {
                        FRAME_INPUT => {
                            let input = Arc::clone(input);
                            tokio::task::spawn_blocking(move || -> Result<()> {
                                let mut input = input.lock().map_err(|_| anyhow::anyhow!("terminal input poisoned"))?;
                                input.write_all(&payload)?; input.flush()?; Ok(())
                            }).await??;
                        }
                        FRAME_CONTROL => {
                            let control: TerminalClientControl = serde_json::from_slice(&payload)?;
                            control.validate()?;
                            if let TerminalClientControl::Resize { cols: next_cols, rows: next_rows } = control {
                                let terminal = Arc::clone(terminal);
                                tokio::task::spawn_blocking(move || terminal.lock().map_err(|_| anyhow::anyhow!("terminal owner poisoned"))?.resize(next_cols, next_rows)).await??;
                                *cols = next_cols; *rows = next_rows;
                            } else { bail!("private terminal does not accept acknowledgements"); }
                        }
                        _ => bail!("invalid private terminal frame"),
                    }
                }
            }
        }
    }.await;
    readers.abort_all();
    while readers.join_next().await.is_some() {}
    result
}
