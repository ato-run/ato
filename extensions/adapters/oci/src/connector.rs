//! How a Run reaches one logical Port of an OCI route.
//!
//! A Port is logical. Where it is physically reachable depends on the host, and
//! that is exactly what must not leak into the rest of the Run: readiness, HTTP
//! and TLS observation, and the Surface forwarder all ask a [`PortConnector`]
//! for a connection and get a byte stream back.
//!
//! Two materializations exist:
//!
//! * [`DirectBridge`] dials the container's address on the Run's internal
//!   network. It is the exact behaviour every native Linux runner already had,
//!   and it is only valid when that address is reachable from this process.
//! * [`crate::relay::DaemonRelay`] starts one relay container on the same
//!   internal network and multiplexes every Port over its stdio. It exists
//!   because a VM-backed daemon (Docker Desktop) and a rootless daemon both put
//!   the container network out of the caller's reach.
//!
//! Which one is used is decided from observed host facts
//! (`ato_host_facts::ConnectorChoice`) and recorded in the Run's evidence. It
//! is never inferred from the argv or from a product name.

use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};

/// How a Port is reached for this Run. Recorded verbatim in the receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorKind {
    /// The container's address is reachable: connect to it.
    Direct,
    /// The container's address is not reachable: multiplex over a relay.
    Relay,
}

impl ConnectorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Relay => "relay",
        }
    }
}

/// The read side of one connected Port, after a split.
pub trait PortRead: Read + Send {}

/// The write side of one connected Port, after a split.
pub trait PortWrite: Write + Send {
    /// Send FIN, keeping the read side open (a TCP half-close).
    fn shutdown_write(&mut self) -> io::Result<()>;
}

/// A bidirectional byte stream to one logical Port.
///
/// `split` exists because a forwarder has to copy in both directions at once:
/// reading one direction while the other blocks must not hold a lock. Each
/// materialization knows how its own transport splits (a socket can be cloned;
/// a relay connection can hand out its outbound frame writer).
pub trait PortConnection: Read + Write + Send {
    /// Send FIN, keeping the read side open (a TCP half-close).
    fn shutdown_write(&mut self) -> io::Result<()>;

    /// Take the two directions apart.
    fn split(self: Box<Self>) -> io::Result<(Box<dyn PortRead>, Box<dyn PortWrite>)>;
}

/// Opens connections to a route's logical Ports.
pub trait PortConnector: Send + Sync {
    fn kind(&self) -> ConnectorKind;

    /// Open one connection. `service` is the container/service name and
    /// `guest_port` the Port's guest port; both come from the selected
    /// Derivation, never from output or argv.
    fn connect(&self, service: &str, guest_port: u16) -> Result<Box<dyn PortConnection>>;
}

/// The native materialization: dial the container's own address.
pub struct DirectBridge {
    target: SocketAddr,
    connect_timeout: Duration,
}

impl DirectBridge {
    pub fn new(address: IpAddr, guest_port: u16) -> Self {
        Self {
            target: SocketAddr::new(address, guest_port),
            connect_timeout: Duration::from_millis(500),
        }
    }

    pub fn target(&self) -> SocketAddr {
        self.target
    }
}

impl PortConnector for DirectBridge {
    fn kind(&self) -> ConnectorKind {
        ConnectorKind::Direct
    }

    fn connect(&self, _service: &str, _guest_port: u16) -> Result<Box<dyn PortConnection>> {
        let stream = TcpStream::connect_timeout(&self.target, self.connect_timeout)
            .with_context(|| format!("connect to container Port {}", self.target))?;
        let _ = stream.set_nodelay(true);
        Ok(Box::new(DirectConnection { stream }))
    }
}

struct DirectConnection {
    stream: TcpStream,
}

impl Read for DirectConnection {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stream.read(buffer)
    }
}

impl Write for DirectConnection {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.stream.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

impl PortConnection for DirectConnection {
    fn shutdown_write(&mut self) -> io::Result<()> {
        self.stream.shutdown(std::net::Shutdown::Write)
    }

    fn split(self: Box<Self>) -> io::Result<(Box<dyn PortRead>, Box<dyn PortWrite>)> {
        let write_side = self.stream.try_clone()?;
        Ok((
            Box::new(DirectRead {
                stream: self.stream,
            }),
            Box::new(DirectWrite { stream: write_side }),
        ))
    }
}

impl PortRead for DirectRead {}

impl Read for DirectRead {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stream.read(buffer)
    }
}

struct DirectRead {
    stream: TcpStream,
}

struct DirectWrite {
    stream: TcpStream,
}

impl PortWrite for DirectWrite {
    fn shutdown_write(&mut self) -> io::Result<()> {
        self.stream.shutdown(std::net::Shutdown::Write)
    }
}

impl Write for DirectWrite {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.stream.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// A connector shared by the readiness probe, the observation and the Surface
/// forwarder of one Run.
pub type SharedConnector = Arc<dyn PortConnector>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn a_direct_connection_reaches_the_listener_it_was_given() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 4];
            stream.read_exact(&mut buffer).unwrap();
            stream.write_all(b"pong").unwrap();
            buffer
        });
        let connector = DirectBridge::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port);
        assert_eq!(connector.kind(), ConnectorKind::Direct);
        let mut connection = connector.connect("any", port).unwrap();
        connection.write_all(b"ping").unwrap();
        connection.flush().unwrap();
        let mut response = [0u8; 4];
        connection.read_exact(&mut response).unwrap();
        assert_eq!(&response, b"pong");
        assert_eq!(&server.join().unwrap(), b"ping");
        connection.shutdown_write().unwrap();
    }
}
