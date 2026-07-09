//! Phase 8a-HW PR B: the guest-agent **AF_VSOCK** listener.
//!
//! In `vsock` mode the agent binds `AF_VSOCK` on a fixed port (default
//! [`DEFAULT_VSOCK_PORT`]) inside the guest and serves the SAME newline-delimited JSON
//! control protocol as stdio mode — the host connects through Firecracker's vsock UDS
//! (`CONNECT <port>`). Only the transport differs; the message handling is identical.

/// Stable guest-agent vsock port (`ATO_GUEST_AGENT_VSOCK_PORT` overrides).
pub const DEFAULT_VSOCK_PORT: u32 = 1025;

#[cfg(target_os = "linux")]
pub fn serve_vsock(
    port: u32,
    mut dispatch: impl FnMut(&str) -> (String, bool),
) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::fd::FromRawFd;

    // socket(AF_VSOCK, SOCK_STREAM, 0)
    let sock = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0) };
    if sock < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let close = |fd: libc::c_int| unsafe { libc::close(fd) };

    // bind (VMADDR_CID_ANY, port)
    let mut addr: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
    addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
    addr.svm_cid = libc::VMADDR_CID_ANY;
    addr.svm_port = port;
    let rc = unsafe {
        libc::bind(
            sock,
            &addr as *const libc::sockaddr_vm as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        let e = std::io::Error::last_os_error();
        close(sock);
        return Err(e);
    }
    if unsafe { libc::listen(sock, 8) } < 0 {
        let e = std::io::Error::last_os_error();
        close(sock);
        return Err(e);
    }

    // Accept connections; each carries the JSON-lines control session. A `Stop` ends
    // the whole agent (the host tears the VM down next).
    let mut stop_all = false;
    while !stop_all {
        let afd = unsafe { libc::accept(sock, std::ptr::null_mut(), std::ptr::null_mut()) };
        if afd < 0 {
            continue;
        }
        // A stream socket fd behaves as a byte stream for File read/write.
        let stream = unsafe { std::fs::File::from_raw_fd(afd) };
        let mut writer = match stream.try_clone() {
            Ok(w) => w,
            Err(_) => continue,
        };
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            if line.trim().is_empty() {
                continue;
            }
            let (resp, stop) = dispatch(&line);
            if writeln!(writer, "{resp}").is_err() {
                break;
            }
            let _ = writer.flush();
            if stop {
                stop_all = true;
                break;
            }
        }
    }
    close(sock);
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn serve_vsock(
    _port: u32,
    _dispatch: impl FnMut(&str) -> (String, bool),
) -> std::io::Result<()> {
    Err(std::io::Error::other("vsock is Linux-only"))
}
