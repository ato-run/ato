#![forbid(unsafe_code)]

use std::{
    env,
    fs::File,
    io::BufReader,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    time::timeout,
};
use tokio_rustls::{
    TlsConnector,
    rustls::{ClientConfig, RootCertStore, pki_types::ServerName},
};
use tracing::{info, warn};
use url::Url;

const DEFAULT_LISTEN: &str = "0.0.0.0:2525";
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;
const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 60;
const DEFAULT_MAX_CONNECTIONS: usize = 32;

#[derive(Clone, Debug)]
struct AdapterConfig {
    listen: SocketAddr,
    socks5: SocketAddrV4,
    target: SocketAddrV4,
    tls_server_name: String,
    ca_pem: PathBuf,
    connect_timeout: Duration,
    idle_timeout: Duration,
    max_connections: usize,
}

impl AdapterConfig {
    fn from_env() -> Result<Self> {
        let listen = env::var("ADAPTER_LISTEN")
            .unwrap_or_else(|_| DEFAULT_LISTEN.to_owned())
            .parse()
            .context("ADAPTER_LISTEN must be a socket address")?;
        let socks5 = parse_socks5_endpoint(&required_env("ATO_BINDING_SMTP_EGRESS")?)?;
        let target: SocketAddrV4 = required_env("SMTP_RELAY_TARGET")?
            .parse()
            .context("SMTP_RELAY_TARGET must be a numeric IPv4 socket address")?;
        ensure_public_target(*target.ip())?;

        let tls_server_name = required_env("SMTP_RELAY_TLS_SERVER_NAME")?;
        ensure!(
            tls_server_name.parse::<Ipv4Addr>().is_err(),
            "SMTP_RELAY_TLS_SERVER_NAME must be a DNS name, not an IP literal"
        );
        ServerName::try_from(tls_server_name.clone())
            .context("SMTP_RELAY_TLS_SERVER_NAME is not a valid TLS server name")?;

        let ca_pem = PathBuf::from(required_env("SMTP_RELAY_CA_PEM")?);
        let connect_timeout =
            duration_env("ADAPTER_CONNECT_TIMEOUT_SECS", DEFAULT_CONNECT_TIMEOUT_SECS)?;
        let idle_timeout = duration_env("ADAPTER_IDLE_TIMEOUT_SECS", DEFAULT_IDLE_TIMEOUT_SECS)?;
        let max_connections =
            positive_usize_env("ADAPTER_MAX_CONNECTIONS", DEFAULT_MAX_CONNECTIONS)?;

        Ok(Self {
            listen,
            socks5,
            target,
            tls_server_name,
            ca_pem,
            connect_timeout,
            idle_timeout,
            max_connections,
        })
    }
}

fn required_env(name: &str) -> Result<String> {
    env::var(name).with_context(|| format!("{name} is required"))
}

fn duration_env(name: &str, default_secs: u64) -> Result<Duration> {
    let seconds = env::var(name)
        .map(|value| {
            value
                .parse::<u64>()
                .with_context(|| format!("{name} must be a positive integer"))
        })
        .unwrap_or(Ok(default_secs))?;
    ensure!(seconds > 0, "{name} must be positive");
    Ok(Duration::from_secs(seconds))
}

fn positive_usize_env(name: &str, default: usize) -> Result<usize> {
    let value = env::var(name)
        .map(|value| {
            value
                .parse::<usize>()
                .with_context(|| format!("{name} must be a positive integer"))
        })
        .unwrap_or(Ok(default))?;
    ensure!(value > 0, "{name} must be positive");
    Ok(value)
}

fn parse_socks5_endpoint(value: &str) -> Result<SocketAddrV4> {
    let url = Url::parse(value).context("ATO_BINDING_SMTP_EGRESS is not a URL")?;
    ensure!(
        url.scheme() == "socks5",
        "egress Binding must use socks5://"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "authenticated SOCKS5 Bindings are unsupported"
    );
    ensure!(
        url.path().is_empty() || url.path() == "/",
        "SOCKS5 Binding URL must not contain a path"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "SOCKS5 Binding URL contains unsupported components"
    );
    let host = url
        .host_str()
        .context("SOCKS5 Binding URL omitted its host")?
        .parse::<Ipv4Addr>()
        .context("SOCKS5 Binding host must be numeric IPv4")?;
    let port = url.port().context("SOCKS5 Binding URL omitted its port")?;
    Ok(SocketAddrV4::new(host, port))
}

fn ensure_public_target(address: Ipv4Addr) -> Result<()> {
    let octets = address.octets();
    let shared = octets[0] == 100 && (64..=127).contains(&octets[1]);
    let benchmark = octets[0] == 198 && matches!(octets[1], 18 | 19);
    let reserved = octets[0] >= 224;
    ensure!(
        !(address.is_unspecified()
            || address.is_loopback()
            || address.is_private()
            || address.is_link_local()
            || address.is_broadcast()
            || shared
            || benchmark
            || reserved),
        "SMTP_RELAY_TARGET must be an allowed public IPv4 address"
    );
    Ok(())
}

fn tls_connector(ca_pem: &PathBuf) -> Result<TlsConnector> {
    let file =
        File::open(ca_pem).with_context(|| format!("open relay CA bundle {}", ca_pem.display()))?;
    let mut reader = BufReader::new(file);
    let mut roots = RootCertStore::empty();
    let mut count = 0usize;
    for certificate in rustls_pemfile::certs(&mut reader) {
        roots
            .add(certificate.context("decode relay CA certificate")?)
            .context("add relay CA certificate")?;
        count += 1;
    }
    ensure!(count > 0, "relay CA bundle contains no certificates");
    Ok(TlsConnector::from(Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )))
}

async fn socks5_connect(config: &AdapterConfig) -> Result<TcpStream> {
    let mut stream = timeout(config.connect_timeout, TcpStream::connect(config.socks5))
        .await
        .context("SOCKS5 broker connect timed out")?
        .context("connect to SOCKS5 broker")?;

    stream
        .write_all(&[5, 1, 0])
        .await
        .context("write SOCKS5 greeting")?;
    let mut method = [0u8; 2];
    stream
        .read_exact(&mut method)
        .await
        .context("read SOCKS5 method")?;
    ensure!(method == [5, 0], "SOCKS5 broker refused no-auth method");

    stream
        .write_all(&socks5_request(config.target))
        .await
        .context("write SOCKS5 CONNECT")?;
    let mut reply = [0u8; 10];
    stream
        .read_exact(&mut reply)
        .await
        .context("read SOCKS5 CONNECT response")?;
    ensure!(
        reply[0] == 5 && reply[2] == 0 && reply[3] == 1,
        "SOCKS5 broker returned an invalid response"
    );
    if reply[1] != 0 {
        bail!("SOCKS5 broker denied fixed target with status {}", reply[1]);
    }
    Ok(stream)
}

fn socks5_request(target: SocketAddrV4) -> [u8; 10] {
    let mut request = [0u8; 10];
    request[..4].copy_from_slice(&[5, 1, 0, 1]);
    request[4..8].copy_from_slice(&target.ip().octets());
    request[8..].copy_from_slice(&target.port().to_be_bytes());
    request
}

async fn serve_connection(
    mut client: TcpStream,
    config: AdapterConfig,
    connector: TlsConnector,
) -> Result<()> {
    let broker_stream = timeout(config.connect_timeout, socks5_connect(&config))
        .await
        .context("SOCKS5 negotiation timed out")??;
    let server_name =
        ServerName::try_from(config.tls_server_name.clone()).context("invalid TLS server name")?;
    let mut relay = timeout(
        config.connect_timeout,
        connector.connect(server_name, broker_stream),
    )
    .await
    .context("relay TLS handshake timed out")?
    .context("relay TLS handshake failed")?;

    let (client_read, client_write) = tokio::io::split(&mut client);
    let (relay_read, relay_write) = tokio::io::split(&mut relay);
    tokio::try_join!(
        copy_with_idle_timeout(client_read, relay_write, config.idle_timeout),
        copy_with_idle_timeout(relay_read, client_write, config.idle_timeout),
    )?;
    Ok(())
}

async fn copy_with_idle_timeout<R, W>(
    mut reader: R,
    mut writer: W,
    idle_timeout: Duration,
) -> Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut copied = 0u64;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let count = timeout(idle_timeout, reader.read(&mut buffer))
            .await
            .context("relay read exceeded its inactivity bound")?
            .context("relay read failed")?;
        if count == 0 {
            writer.shutdown().await.context("relay half-close failed")?;
            return Ok(copied);
        }
        timeout(idle_timeout, writer.write_all(&buffer[..count]))
            .await
            .context("relay write exceeded its inactivity bound")?
            .context("relay write failed")?;
        copied += count as u64;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    let config = AdapterConfig::from_env()?;
    let connector = tls_connector(&config.ca_pem)?;
    let listener = TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("bind transport Adapter on {}", config.listen))?;
    let slots = Arc::new(Semaphore::new(config.max_connections));
    info!(
        listen = %config.listen,
        relay_target = %config.target,
        tls_server_name = %config.tls_server_name,
        max_connections = config.max_connections,
        "fixed relay transport Adapter ready"
    );

    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.context("listen for shutdown signal")?;
                info!("shutdown signal received");
                return Ok(());
            }
            accepted = listener.accept() => {
                let (client, peer) = accepted.context("accept internal relay connection")?;
                let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else {
                    warn!(%peer, "connection limit reached");
                    continue;
                };
                let connection_config = config.clone();
                let connection_connector = connector.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(error) = serve_connection(client, connection_config, connection_connector).await {
                        warn!(%peer, error = %error, "relay connection ended with an error");
                    }
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_a_numeric_no_auth_socks5_binding() {
        assert_eq!(
            parse_socks5_endpoint("socks5://172.18.0.1:1080").unwrap(),
            "172.18.0.1:1080".parse().unwrap()
        );
        for rejected in [
            "http://172.18.0.1:1080",
            "socks5://user:secret@172.18.0.1:1080",
            "socks5://broker:1080",
            "socks5://172.18.0.1",
            "socks5://172.18.0.1:1080/path",
        ] {
            assert!(parse_socks5_endpoint(rejected).is_err(), "{rejected}");
        }
    }

    #[test]
    fn request_can_only_name_the_configured_ipv4_target() {
        assert_eq!(
            socks5_request("203.0.113.9:2465".parse().unwrap()),
            [5, 1, 0, 1, 203, 0, 113, 9, 9, 161]
        );
    }

    #[test]
    fn rejects_non_public_targets_before_opening_a_socket() {
        for address in [
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::new(10, 0, 0, 1),
            Ipv4Addr::new(100, 64, 0, 1),
            Ipv4Addr::new(169, 254, 169, 254),
            Ipv4Addr::new(198, 18, 0, 1),
            Ipv4Addr::new(224, 0, 0, 1),
        ] {
            assert!(ensure_public_target(address).is_err(), "{address}");
        }
        assert!(ensure_public_target(Ipv4Addr::new(167, 234, 219, 224)).is_ok());
    }
}
