//! The wire format between a Run and its relay container.
//!
//! This module is compiled twice: as part of the adapter (the Run's end) and
//! by the relay binary itself (`src/bin/ato-oci-relay.rs`), which includes the
//! file directly so that the static binary does not link the adapter.
//!
//! A Run whose Docker daemon keeps the container network out of reach (a
//! VM-backed daemon, or a rootless one) cannot dial a container's IP. It can
//! still reach the network from *inside* it, through one relay container it
//! starts on the same internal network. That container carries this protocol
//! over its own stdin/stdout: `docker run -i` gives one long-lived bidirectional
//! stream, and every logical Port is a connection multiplexed on it.
//!
//! Framing is deliberately trivial so that both ends can be plain `std`:
//!
//! ```text
//! kind: u8   conn: u32 LE   len: u32 LE   payload: [len]
//! ```
//!
//! The relay never chooses a destination on its own: the Run opens a
//! connection to a `(service, guest_port)` pair it listed on the relay's
//! command line, and the relay refuses anything else.
//!
//! No dependency is used here. The relay is compiled to a static Linux binary
//! and embedded in the CLI, so it must build with nothing but `std`.

use std::io::{self, Read, Write};

/// The protocol version carried in the handshake.
pub const PROTOCOL: &str = "ato-oci-relay/1";

/// One frame on the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// Host → relay: open a connection to a declared destination.
    Open {
        conn: u32,
        service: String,
        port: u16,
    },
    /// Relay → host: the destination accepted a connection.
    Opened { conn: u32 },
    /// Relay → host: the destination could not be reached; the connection is
    /// not open and no data will follow.
    OpenFailed { conn: u32, reason: String },
    /// Either direction: bytes for one connection.
    Data { conn: u32, bytes: Vec<u8> },
    /// Either direction: the sender will send no more bytes for this
    /// connection. The other direction stays open (a TCP half-close).
    HalfClose { conn: u32 },
    /// Either direction: this connection is finished.
    Close { conn: u32 },
    /// Relay → host, first frame: which protocol it speaks.
    Hello { version: String },
}

/// Frame kind tags. Stable: they are on the wire.
const KIND_OPEN: u8 = 1;
const KIND_OPENED: u8 = 2;
const KIND_OPEN_FAILED: u8 = 3;
const KIND_DATA: u8 = 4;
const KIND_HALF_CLOSE: u8 = 5;
const KIND_CLOSE: u8 = 6;
const KIND_HELLO: u8 = 7;

/// The largest payload this protocol will carry in one frame.
///
/// A bigger write is split by the writer; a bigger frame is a protocol error
/// rather than an allocation the peer chose.
pub const MAX_FRAME_PAYLOAD: usize = 64 * 1024;

impl Frame {
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let (kind, conn, payload) = match self {
            Self::Open {
                conn,
                service,
                port,
            } => {
                let mut payload = Vec::with_capacity(service.len() + 3);
                payload.extend_from_slice(service.as_bytes());
                payload.push(0);
                payload.extend_from_slice(&port.to_le_bytes());
                (KIND_OPEN, *conn, payload)
            }
            Self::Opened { conn } => (KIND_OPENED, *conn, Vec::new()),
            Self::OpenFailed { conn, reason } => {
                (KIND_OPEN_FAILED, *conn, reason.as_bytes().to_vec())
            }
            Self::Data { conn, bytes } => (KIND_DATA, *conn, bytes.clone()),
            Self::HalfClose { conn } => (KIND_HALF_CLOSE, *conn, Vec::new()),
            Self::Close { conn } => (KIND_CLOSE, *conn, Vec::new()),
            Self::Hello { version } => (KIND_HELLO, 0, version.as_bytes().to_vec()),
        };
        if payload.len() > MAX_FRAME_PAYLOAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "relay frame payload exceeds the protocol bound",
            ));
        }
        let mut encoded = Vec::with_capacity(9 + payload.len());
        encoded.push(kind);
        encoded.extend_from_slice(&conn.to_le_bytes());
        encoded.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        encoded.extend_from_slice(&payload);
        Ok(encoded)
    }

    /// Read one frame. `Ok(None)` is a clean end of stream.
    pub fn decode(reader: &mut impl Read) -> io::Result<Option<Self>> {
        let mut header = [0u8; 9];
        if !read_exact_or_eof(reader, &mut header)? {
            return Ok(None);
        }
        let kind = header[0];
        let conn = u32::from_le_bytes([header[1], header[2], header[3], header[4]]);
        let len = u32::from_le_bytes([header[5], header[6], header[7], header[8]]) as usize;
        if len > MAX_FRAME_PAYLOAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "relay frame payload exceeds the protocol bound",
            ));
        }
        let mut payload = vec![0u8; len];
        if len > 0 {
            reader.read_exact(&mut payload)?;
        }
        let frame = match kind {
            KIND_OPEN => {
                let split = payload.iter().position(|byte| *byte == 0).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "relay Open frame has no service",
                    )
                })?;
                let service = String::from_utf8(payload[..split].to_vec()).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "relay service is not utf8")
                })?;
                let port_bytes = payload.get(split + 1..split + 3).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "relay Open frame has no port")
                })?;
                Self::Open {
                    conn,
                    service,
                    port: u16::from_le_bytes([port_bytes[0], port_bytes[1]]),
                }
            }
            KIND_OPENED => Self::Opened { conn },
            KIND_OPEN_FAILED => Self::OpenFailed {
                conn,
                reason: String::from_utf8_lossy(&payload).into_owned(),
            },
            KIND_DATA => Self::Data {
                conn,
                bytes: payload,
            },
            KIND_HALF_CLOSE => Self::HalfClose { conn },
            KIND_CLOSE => Self::Close { conn },
            KIND_HELLO => Self::Hello {
                version: String::from_utf8_lossy(&payload).into_owned(),
            },
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unknown relay frame kind {other}"),
                ));
            }
        };
        Ok(Some(frame))
    }

    /// Write one frame, splitting nothing: `Data` larger than the bound is the
    /// caller's error, and [`write_data`] is what splits.
    pub fn write_to(&self, writer: &mut impl Write) -> io::Result<()> {
        let encoded = self.encode()?;
        writer.write_all(&encoded)?;
        writer.flush()
    }
}

/// Write `bytes` as one or more `Data` frames of at most [`MAX_FRAME_PAYLOAD`].
pub fn write_data(writer: &mut impl Write, conn: u32, bytes: &[u8]) -> io::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    for chunk in bytes.chunks(MAX_FRAME_PAYLOAD) {
        Frame::Data {
            conn,
            bytes: chunk.to_vec(),
        }
        .write_to(writer)?;
    }
    Ok(())
}

fn read_exact_or_eof(reader: &mut impl Read, buffer: &mut [u8]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => {
                if filled == 0 {
                    return Ok(false);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "relay stream ended in the middle of a frame",
                ));
            }
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(true)
}

/// Resolve a container name with this process's own DNS client.
///
/// The relay is a static binary, and a static musl `getaddrinfo` does not
/// complete against Docker's embedded resolver (it times out and reports
/// `EAI_AGAIN`), while the same query from a libc-linked container succeeds.
/// Since the relay has to resolve the peers of the network it is on, it asks
/// the same nameserver directly, over a UDP A query it builds itself, instead
/// of depending on how its own libc was linked.
///
/// Returns the address records in the order the server listed them.
pub fn resolve_ipv4(name: &str, resolv_conf: &str) -> Result<Vec<std::net::Ipv4Addr>, String> {
    let server =
        nameserver(resolv_conf).ok_or_else(|| "no nameserver in /etc/resolv.conf".to_owned())?;
    let query = dns_query(name, 0x2f1a)?;
    let socket = std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0))
        .map_err(|error| format!("bind DNS probe socket: {error}"))?;
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .map_err(|error| format!("set DNS probe timeout: {error}"))?;
    socket
        .send_to(&query, (server, 53))
        .map_err(|error| format!("send DNS query for {name}: {error}"))?;
    let mut response = [0u8; 1500];
    let (read, _) = socket
        .recv_from(&mut response)
        .map_err(|error| format!("DNS query for {name} timed out: {error}"))?;
    parse_a_records(&response[..read])
}

/// The first IPv4 nameserver a resolv.conf lists.
pub fn nameserver(resolv_conf: &str) -> Option<std::net::Ipv4Addr> {
    resolv_conf
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.strip_prefix("nameserver "))
        .filter_map(|value| value.trim().parse().ok())
        .next()
}

fn dns_query(name: &str, id: u16) -> Result<Vec<u8>, String> {
    let mut query = Vec::with_capacity(64);
    query.extend_from_slice(&id.to_be_bytes());
    query.extend_from_slice(&0x0100u16.to_be_bytes()); // recursion desired
    query.extend_from_slice(&1u16.to_be_bytes()); // questions
    query.extend_from_slice(&0u16.to_be_bytes()); // answers
    query.extend_from_slice(&0u16.to_be_bytes()); // authority
    query.extend_from_slice(&0u16.to_be_bytes()); // additional
    if name.is_empty() || name.len() > 253 {
        return Err(format!("{name:?} is not a DNS name"));
    }
    for label in name.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(format!("{name:?} is not a DNS name"));
        }
        query.push(label.len() as u8);
        query.extend_from_slice(label.as_bytes());
    }
    query.push(0);
    query.extend_from_slice(&1u16.to_be_bytes()); // A
    query.extend_from_slice(&1u16.to_be_bytes()); // IN
    Ok(query)
}

fn parse_a_records(response: &[u8]) -> Result<Vec<std::net::Ipv4Addr>, String> {
    if response.len() < 12 {
        return Err("DNS response is truncated".to_owned());
    }
    let rcode = response[3] & 0x0f;
    if rcode == 3 {
        return Err("DNS name does not exist".to_owned());
    }
    if rcode != 0 {
        return Err(format!("DNS server returned rcode {rcode}"));
    }
    let questions = u16::from_be_bytes([response[4], response[5]]) as usize;
    let answers = u16::from_be_bytes([response[6], response[7]]) as usize;
    let mut offset = 12;
    for _ in 0..questions {
        offset = skip_name(response, offset)?;
        offset = offset
            .checked_add(4)
            .filter(|offset| *offset <= response.len())
            .ok_or_else(|| "DNS question is truncated".to_owned())?;
    }
    let mut records = Vec::new();
    for _ in 0..answers {
        offset = skip_name(response, offset)?;
        if offset + 10 > response.len() {
            return Err("DNS answer is truncated".to_owned());
        }
        let record_type = u16::from_be_bytes([response[offset], response[offset + 1]]);
        let record_class = u16::from_be_bytes([response[offset + 2], response[offset + 3]]);
        let length = u16::from_be_bytes([response[offset + 8], response[offset + 9]]) as usize;
        offset += 10;
        if offset + length > response.len() {
            return Err("DNS answer data is truncated".to_owned());
        }
        if record_type == 1 && record_class == 1 && length == 4 {
            records.push(std::net::Ipv4Addr::new(
                response[offset],
                response[offset + 1],
                response[offset + 2],
                response[offset + 3],
            ));
        }
        offset += length;
    }
    if records.is_empty() {
        return Err("DNS response carried no address record".to_owned());
    }
    Ok(records)
}

/// Skip a (possibly compressed) DNS name, returning the offset after it.
fn skip_name(message: &[u8], mut offset: usize) -> Result<usize, String> {
    loop {
        let Some(&length) = message.get(offset) else {
            return Err("DNS name is truncated".to_owned());
        };
        if length == 0 {
            return Ok(offset + 1);
        }
        if length & 0xc0 == 0xc0 {
            // A compression pointer is two bytes and ends this name.
            return Ok(offset + 2);
        }
        offset += 1 + length as usize;
        if offset > message.len() {
            return Err("DNS name is truncated".to_owned());
        }
    }
}

/// A destination the relay is permitted to reach.
///
/// The list comes from the Run's own declaration (service, guest port). A pair
/// that is not in it is refused before any socket is opened, so the relay
/// cannot be turned into a general purpose proxy into the Run's network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedDestination {
    pub service: String,
    pub port: u16,
}

impl AllowedDestination {
    /// Parse `service:port`, the form the Run passes on the relay's argv.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let (service, port) = raw
            .rsplit_once(':')
            .ok_or_else(|| format!("relay destination {raw:?} is not service:port"))?;
        if service.is_empty() {
            return Err(format!("relay destination {raw:?} has no service"));
        }
        let port: u16 = port
            .parse()
            .map_err(|_| format!("relay destination {raw:?} has no numeric port"))?;
        Ok(Self {
            service: service.to_owned(),
            port,
        })
    }

    pub fn admits(&self, service: &str, port: u16) -> bool {
        self.service == service && self.port == port
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(frame: Frame) {
        let encoded = frame.encode().unwrap();
        let decoded = Frame::decode(&mut &encoded[..]).unwrap().unwrap();
        assert_eq!(decoded, frame);
    }

    #[test]
    fn every_frame_round_trips() {
        round_trip(Frame::Hello {
            version: PROTOCOL.to_owned(),
        });
        round_trip(Frame::Open {
            conn: 7,
            service: "backend".to_owned(),
            port: 8081,
        });
        round_trip(Frame::Opened { conn: 7 });
        round_trip(Frame::OpenFailed {
            conn: 7,
            reason: "connection refused".to_owned(),
        });
        round_trip(Frame::Data {
            conn: 7,
            bytes: vec![0, 1, 2, 255],
        });
        round_trip(Frame::HalfClose { conn: 7 });
        round_trip(Frame::Close { conn: 7 });
    }

    #[test]
    fn a_stream_of_frames_decodes_in_order() {
        let mut stream = Vec::new();
        Frame::Hello {
            version: PROTOCOL.to_owned(),
        }
        .write_to(&mut stream)
        .unwrap();
        Frame::Opened { conn: 1 }.write_to(&mut stream).unwrap();
        Frame::Data {
            conn: 1,
            bytes: b"ok".to_vec(),
        }
        .write_to(&mut stream)
        .unwrap();
        let mut reader = &stream[..];
        assert_eq!(
            Frame::decode(&mut reader).unwrap(),
            Some(Frame::Hello {
                version: PROTOCOL.to_owned()
            })
        );
        assert_eq!(
            Frame::decode(&mut reader).unwrap(),
            Some(Frame::Opened { conn: 1 })
        );
        assert_eq!(
            Frame::decode(&mut reader).unwrap(),
            Some(Frame::Data {
                conn: 1,
                bytes: b"ok".to_vec()
            })
        );
        assert_eq!(Frame::decode(&mut reader).unwrap(), None);
    }

    #[test]
    fn large_writes_are_split_into_bounded_frames() {
        let payload = vec![7u8; MAX_FRAME_PAYLOAD * 2 + 1];
        let mut stream = Vec::new();
        write_data(&mut stream, 3, &payload).unwrap();
        let mut reader = &stream[..];
        let mut reassembled = Vec::new();
        loop {
            match Frame::decode(&mut reader).unwrap() {
                Some(Frame::Data { conn: 3, bytes }) => reassembled.extend_from_slice(&bytes),
                Some(other) => panic!("unexpected frame {other:?}"),
                None => break,
            }
        }
        assert_eq!(reassembled, payload);
    }

    #[test]
    fn a_truncated_frame_is_an_error_not_a_connection() {
        let mut encoded = Frame::Data {
            conn: 1,
            bytes: b"hello".to_vec(),
        }
        .encode()
        .unwrap();
        encoded.truncate(encoded.len() - 2);
        assert!(Frame::decode(&mut &encoded[..]).is_err());
    }

    #[test]
    fn an_oversized_claimed_length_is_refused_before_allocating() {
        let mut header = vec![KIND_DATA, 1, 0, 0, 0];
        header.extend_from_slice(&(MAX_FRAME_PAYLOAD as u32 + 1).to_le_bytes());
        assert!(Frame::decode(&mut &header[..]).is_err());
    }

    #[test]
    fn a_dns_a_record_answer_is_parsed_without_the_system_resolver() {
        // A response for `web` with one CNAME and one A record, as Docker's
        // embedded resolver sends it.
        let mut response = Vec::new();
        response.extend_from_slice(&0x2f1au16.to_be_bytes());
        response.extend_from_slice(&0x8180u16.to_be_bytes());
        response.extend_from_slice(&1u16.to_be_bytes());
        response.extend_from_slice(&2u16.to_be_bytes());
        response.extend_from_slice(&0u16.to_be_bytes());
        response.extend_from_slice(&0u16.to_be_bytes());
        response.extend_from_slice(&[3, b'w', b'e', b'b', 0]);
        response.extend_from_slice(&[0, 1, 0, 1]);
        // CNAME web -> a1b2c3d4
        response.extend_from_slice(&[0xc0, 12]);
        response.extend_from_slice(&[0, 5, 0, 1]);
        response.extend_from_slice(&0u32.to_be_bytes());
        response.extend_from_slice(&[0, 10]);
        response.extend_from_slice(&[8, b'a', b'1', b'b', b'2', b'c', b'3', b'd', b'4', 0]);
        // A 172.28.0.2
        response.extend_from_slice(&[0xc0, 12]);
        response.extend_from_slice(&[0, 1, 0, 1]);
        response.extend_from_slice(&0u32.to_be_bytes());
        response.extend_from_slice(&[0, 4]);
        response.extend_from_slice(&[172, 28, 0, 2]);
        assert_eq!(
            parse_a_records(&response).unwrap(),
            vec![std::net::Ipv4Addr::new(172, 28, 0, 2)]
        );
    }

    #[test]
    fn nxdname_is_an_error_not_an_ip() {
        let mut response = vec![0x2f, 0x1a, 0x81, 0x83, 0, 0, 0, 0, 0, 0, 0, 0];
        let _ = &mut response;
        assert!(parse_a_records(&response).is_err());
    }

    #[test]
    fn a_container_nameserver_is_read_from_resolv_conf() {
        let conf = "# generated\nnameserver 127.0.0.11\noptions ndots:0\n";
        assert_eq!(
            nameserver(conf),
            Some(std::net::Ipv4Addr::new(127, 0, 0, 11))
        );
        assert_eq!(nameserver("options ndots:0\n"), None);
        let query = dns_query("web", 1).unwrap();
        assert_eq!(&query[12..18], &[3, b'w', b'e', b'b', 0, 0]);
        assert!(dns_query("bad..name", 1).is_err());
    }

    #[test]
    fn destinations_are_exact_pairs() {
        let destination = AllowedDestination::parse("web:8080").unwrap();
        assert!(destination.admits("web", 8080));
        assert!(!destination.admits("web", 8081));
        assert!(!destination.admits("backend", 8080));
        assert!(!destination.admits("web ", 8080));
        assert!(AllowedDestination::parse("web").is_err());
        assert!(AllowedDestination::parse(":8080").is_err());
        assert!(AllowedDestination::parse("web:http").is_err());
    }
}
