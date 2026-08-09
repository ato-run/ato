//! Dedicated guest terminal stream on AF_VSOCK port 1026.

use std::io;
#[cfg(target_os = "linux")]
use std::io::{Read, Write};
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::sync::Mutex;

#[cfg(target_os = "linux")]
use protocol::terminal_surface::{
    MAX_TERMINAL_CONTROL_FRAME_BYTES, MAX_TERMINAL_INPUT_FRAME_BYTES, TerminalClientControl,
    TerminalServerControl,
};

use crate::terminal::TerminalBrokerState;

pub const DEFAULT_TERMINAL_VSOCK_PORT: u32 = 1026;

#[cfg(target_os = "linux")]
const FRAME_INPUT: u8 = 1;
#[cfg(target_os = "linux")]
const FRAME_OUTPUT: u8 = 2;
#[cfg(target_os = "linux")]
const FRAME_CONTROL: u8 = 3;

#[cfg(target_os = "linux")]
pub fn serve_terminal_vsock(port: u32, state: Arc<TerminalBrokerState>) -> io::Result<()> {
    use std::os::fd::FromRawFd;
    let sock = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0) };
    if sock < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut addr: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
    addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
    addr.svm_cid = libc::VMADDR_CID_ANY;
    addr.svm_port = port;
    if unsafe {
        libc::bind(
            sock,
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
        )
    } < 0
        || unsafe { libc::listen(sock, 1) } < 0
    {
        let error = io::Error::last_os_error();
        unsafe {
            libc::close(sock);
        }
        return Err(error);
    }
    loop {
        let fd = unsafe { libc::accept(sock, std::ptr::null_mut(), std::ptr::null_mut()) };
        if fd < 0 {
            continue;
        }
        let stream = unsafe { File::from_raw_fd(fd) };
        if let Err(error) = serve_terminal_stream(stream, Arc::clone(&state)) {
            eprintln!("ato-guest-agent: terminal attach ended: {error}");
        }
    }
}

#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;

#[cfg(not(target_os = "linux"))]
pub fn serve_terminal_vsock(_port: u32, _state: Arc<TerminalBrokerState>) -> io::Result<()> {
    Err(io::Error::other("terminal vsock is Linux-only"))
}

#[cfg(target_os = "linux")]
fn serve_terminal_stream(mut stream: File, state: Arc<TerminalBrokerState>) -> io::Result<()> {
    let attachment = state.attach()?;
    let generation = attachment.generation;
    let writer = Arc::new(Mutex::new(stream.try_clone()?));
    write_control(
        &writer,
        &TerminalServerControl::Ready {
            cols: attachment.cols,
            rows: attachment.rows,
        },
    )?;

    let mut output = attachment.master.try_clone()?;
    let output_writer = Arc::clone(&writer);
    let output_state = Arc::clone(&state);
    std::thread::spawn(move || {
        let mut buffer = vec![0u8; protocol::terminal_surface::MAX_TERMINAL_OUTPUT_CHUNK_BYTES];
        while let Ok(count) = output.read(&mut buffer) {
            if count == 0 {
                break;
            }
            if write_frame(&output_writer, FRAME_OUTPUT, &buffer[..count]).is_err() {
                return;
            }
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let exit = loop {
            if let Some(exit) = output_state.exit_for(generation) {
                break Some(exit);
            }
            if std::time::Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        match exit {
            Some(exit) => {
                let _ = write_control(
                    &output_writer,
                    &TerminalServerControl::Exit {
                        code: exit.code,
                        signal: exit.signal,
                    },
                );
            }
            None => {
                let _ = write_control(
                    &output_writer,
                    &TerminalServerControl::Error {
                        code: "terminal_status_unavailable".into(),
                        message: "terminal workload ended without an exit status".into(),
                    },
                );
            }
        }
        if let Ok(writer) = output_writer.lock() {
            unsafe {
                libc::shutdown(writer.as_raw_fd(), libc::SHUT_RDWR);
            }
        }
    });

    let mut master_input = attachment.master.try_clone()?;
    loop {
        let (kind, payload) = read_frame(&mut stream)?;
        match kind {
            FRAME_INPUT if payload.len() <= MAX_TERMINAL_INPUT_FRAME_BYTES => {
                master_input.write_all(&payload)?
            }
            FRAME_CONTROL if payload.len() <= MAX_TERMINAL_CONTROL_FRAME_BYTES => {
                let control: TerminalClientControl = serde_json::from_slice(&payload)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                control
                    .validate()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
                if let TerminalClientControl::Resize { cols, rows } = control {
                    state.resize(generation, cols, rows)?;
                }
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid terminal frame",
                ));
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn read_frame(reader: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 5];
    reader.read_exact(&mut header)?;
    let len = u32::from_be_bytes(header[1..5].try_into().expect("four bytes")) as usize;
    let limit = MAX_TERMINAL_INPUT_FRAME_BYTES.max(MAX_TERMINAL_CONTROL_FRAME_BYTES);
    if len > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "terminal frame exceeds limit",
        ));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    Ok((header[0], payload))
}

#[cfg(target_os = "linux")]
fn write_control<W: Write>(
    writer: &Arc<Mutex<W>>,
    control: &TerminalServerControl,
) -> io::Result<()> {
    let payload = serde_json::to_vec(control).map_err(io::Error::other)?;
    write_frame(writer, FRAME_CONTROL, &payload)
}

#[cfg(target_os = "linux")]
fn write_frame<W: Write>(writer: &Arc<Mutex<W>>, kind: u8, payload: &[u8]) -> io::Result<()> {
    let mut writer = writer
        .lock()
        .map_err(|_| io::Error::other("terminal stream lock poisoned"))?;
    writer.write_all(&[kind])?;
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(payload)?;
    writer.flush()
}
