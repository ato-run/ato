//! Private RFB readiness probe used before a pixel surface is reported ready.
//!
//! The probe negotiates RFB 3.8 with the unauthenticated *private* guest
//! endpoint, requests the raw encoding, and waits for a complete framebuffer
//! update. Public authorization remains the host gateway's responsibility.

use std::{net::SocketAddr, time::Duration};

use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};

const RFB_3_8_VERSION: &[u8; 12] = b"RFB 003.008\n";
const SECURITY_TYPE_NONE: u8 = 1;
const ENCODING_RAW: i32 = 0;
const MAX_DESKTOP_NAME_BYTES: usize = 4 * 1024;
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RfbFrameInfo {
    pub width: u16,
    pub height: u16,
    pub bytes: usize,
}

#[derive(Debug, Error)]
pub enum RfbProbeError {
    #[error("RFB readiness probe timed out")]
    Timeout,
    #[error("RFB readiness probe I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("RFB server protocol is unsupported")]
    UnsupportedProtocol,
    #[error("RFB private endpoint did not offer security type None")]
    UnsupportedSecurity,
    #[error("RFB private endpoint rejected initialization")]
    SecurityRejected,
    #[error("RFB framebuffer metadata is invalid")]
    InvalidFramebuffer,
}

/// Connects to a private RFB endpoint and waits for its first complete raw
/// framebuffer update. The timeout covers connect, handshake, and frame.
pub async fn wait_for_first_rfb_frame(
    addr: SocketAddr,
    wait: Duration,
) -> Result<RfbFrameInfo, RfbProbeError> {
    timeout(wait, probe(addr))
        .await
        .map_err(|_| RfbProbeError::Timeout)?
}

async fn probe(addr: SocketAddr) -> Result<RfbFrameInfo, RfbProbeError> {
    let mut stream = TcpStream::connect(addr).await?;

    let mut version = [0_u8; 12];
    stream.read_exact(&mut version).await?;
    if &version != RFB_3_8_VERSION {
        return Err(RfbProbeError::UnsupportedProtocol);
    }
    stream.write_all(RFB_3_8_VERSION).await?;

    let security_count = stream.read_u8().await? as usize;
    if security_count == 0 || security_count > u8::MAX as usize {
        return Err(RfbProbeError::UnsupportedSecurity);
    }
    let mut security_types = vec![0_u8; security_count];
    stream.read_exact(&mut security_types).await?;
    if !security_types.contains(&SECURITY_TYPE_NONE) {
        return Err(RfbProbeError::UnsupportedSecurity);
    }
    stream.write_u8(SECURITY_TYPE_NONE).await?;
    if stream.read_u32().await? != 0 {
        return Err(RfbProbeError::SecurityRejected);
    }

    // Shared flag keeps the long-lived x11vnc server available for the actual
    // browser gateway connection after this short readiness probe exits.
    stream.write_u8(1).await?;
    let mut server_init = [0_u8; 24];
    stream.read_exact(&mut server_init).await?;
    let width = u16::from_be_bytes([server_init[0], server_init[1]]);
    let height = u16::from_be_bytes([server_init[2], server_init[3]]);
    let bytes_per_pixel = usize::from(server_init[4]).checked_div(8).unwrap_or(0);
    let name_len = u32::from_be_bytes([
        server_init[20],
        server_init[21],
        server_init[22],
        server_init[23],
    ]) as usize;
    if width == 0
        || height == 0
        || !matches!(bytes_per_pixel, 1 | 2 | 4)
        || name_len > MAX_DESKTOP_NAME_BYTES
    {
        return Err(RfbProbeError::InvalidFramebuffer);
    }
    let mut desktop_name = vec![0_u8; name_len];
    stream.read_exact(&mut desktop_name).await?;

    // SetEncodings(raw), then request one full non-incremental update.
    stream.write_all(&[2, 0, 0, 1]).await?;
    stream.write_all(&ENCODING_RAW.to_be_bytes()).await?;
    let [width_hi, width_lo] = width.to_be_bytes();
    let [height_hi, height_lo] = height.to_be_bytes();
    stream
        .write_all(&[3, 0, 0, 0, 0, 0, width_hi, width_lo, height_hi, height_lo])
        .await?;

    loop {
        match stream.read_u8().await? {
            0 => {
                let _padding = stream.read_u8().await?;
                let rectangle_count = stream.read_u16().await?;
                if rectangle_count == 0 {
                    continue;
                }
                let mut frame_bytes = 0_usize;
                for _ in 0..rectangle_count {
                    let mut rectangle = [0_u8; 12];
                    stream.read_exact(&mut rectangle).await?;
                    let x = u16::from_be_bytes([rectangle[0], rectangle[1]]);
                    let y = u16::from_be_bytes([rectangle[2], rectangle[3]]);
                    let rect_width = u16::from_be_bytes([rectangle[4], rectangle[5]]);
                    let rect_height = u16::from_be_bytes([rectangle[6], rectangle[7]]);
                    let encoding = i32::from_be_bytes([
                        rectangle[8],
                        rectangle[9],
                        rectangle[10],
                        rectangle[11],
                    ]);
                    if encoding != ENCODING_RAW {
                        return Err(RfbProbeError::UnsupportedProtocol);
                    }
                    if rect_width == 0
                        || rect_height == 0
                        || u32::from(x) + u32::from(rect_width) > u32::from(width)
                        || u32::from(y) + u32::from(rect_height) > u32::from(height)
                    {
                        return Err(RfbProbeError::InvalidFramebuffer);
                    }
                    let rectangle_bytes = usize::from(rect_width)
                        .checked_mul(usize::from(rect_height))
                        .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
                        .ok_or(RfbProbeError::InvalidFramebuffer)?;
                    frame_bytes = frame_bytes
                        .checked_add(rectangle_bytes)
                        .filter(|bytes| *bytes <= MAX_FRAME_BYTES)
                        .ok_or(RfbProbeError::InvalidFramebuffer)?;
                    let mut rectangle_pixels = vec![0_u8; rectangle_bytes];
                    stream.read_exact(&mut rectangle_pixels).await?;
                }
                return Ok(RfbFrameInfo {
                    width,
                    height,
                    bytes: frame_bytes,
                });
            }
            2 => {}
            3 => {
                let mut header = [0_u8; 7];
                stream.read_exact(&mut header).await?;
                let length =
                    u32::from_be_bytes([header[3], header[4], header[5], header[6]]) as usize;
                if length > MAX_DESKTOP_NAME_BYTES {
                    return Err(RfbProbeError::UnsupportedProtocol);
                }
                let mut text = vec![0_u8; length];
                stream.read_exact(&mut text).await?;
            }
            _ => return Err(RfbProbeError::UnsupportedProtocol),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    async fn fixture_server(security_types: Vec<u8>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(RFB_3_8_VERSION).await.unwrap();
            let mut version = [0_u8; 12];
            stream.read_exact(&mut version).await.unwrap();
            stream.write_u8(security_types.len() as u8).await.unwrap();
            stream.write_all(&security_types).await.unwrap();
            if !security_types.contains(&SECURITY_TYPE_NONE) {
                return;
            }
            assert_eq!(stream.read_u8().await.unwrap(), SECURITY_TYPE_NONE);
            stream.write_u32(0).await.unwrap();
            assert_eq!(stream.read_u8().await.unwrap(), 1);

            let mut server_init = Vec::new();
            server_init.extend_from_slice(&2_u16.to_be_bytes());
            server_init.extend_from_slice(&1_u16.to_be_bytes());
            server_init
                .extend_from_slice(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
            server_init.extend_from_slice(&7_u32.to_be_bytes());
            server_init.extend_from_slice(b"fixture");
            stream.write_all(&server_init).await.unwrap();

            let mut client_requests = [0_u8; 18];
            stream.read_exact(&mut client_requests).await.unwrap();
            assert_eq!(&client_requests[..8], &[2, 0, 0, 1, 0, 0, 0, 0]);
            assert_eq!(&client_requests[8..], &[3, 0, 0, 0, 0, 0, 0, 2, 0, 1]);

            stream.write_all(&[0, 0, 0, 2]).await.unwrap();
            stream
                .write_all(&[0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0])
                .await
                .unwrap();
            stream.write_all(&[0_u8; 4]).await.unwrap();
            stream
                .write_all(&[0, 1, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0])
                .await
                .unwrap();
            stream.write_all(&[0_u8; 4]).await.unwrap();
        });
        addr
    }

    #[tokio::test]
    async fn waits_for_a_complete_raw_frame() {
        let addr = fixture_server(vec![SECURITY_TYPE_NONE]).await;

        let frame = wait_for_first_rfb_frame(addr, Duration::from_secs(1))
            .await
            .unwrap();

        assert_eq!(
            frame,
            RfbFrameInfo {
                width: 2,
                height: 1,
                bytes: 8,
            }
        );
    }

    #[tokio::test]
    async fn rejects_password_only_private_endpoints() {
        let addr = fixture_server(vec![2]).await;

        assert!(matches!(
            wait_for_first_rfb_frame(addr, Duration::from_secs(1)).await,
            Err(RfbProbeError::UnsupportedSecurity)
        ));
    }
}
