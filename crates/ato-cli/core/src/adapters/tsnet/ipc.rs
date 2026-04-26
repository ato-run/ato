#[cfg(unix)]
use std::path::PathBuf;

use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite};
#[cfg(unix)]
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint};
use tower::service_fn;

use crate::error::{CapsuleError, Result};
use crate::tsnet::proto::tsnet_service_client::TsnetServiceClient;
use crate::tsnet::TsnetEndpoint;

#[derive(Debug, Clone)]
pub enum IpcTransport {
    #[cfg(unix)]
    Uds(PathBuf),
    #[cfg(windows)]
    NamedPipe(String),
}

impl IpcTransport {
    pub fn from_env() -> Result<Self> {
        #[cfg(unix)]
        {
            let value = std::env::var("ATO_TSNET_GRPC_SOCKET")
                .map_err(|_| CapsuleError::Config("Missing ATO_TSNET_GRPC_SOCKET".to_string()))?;
            Ok(IpcTransport::Uds(PathBuf::from(value)))
        }

        #[cfg(windows)]
        {
            let value = std::env::var("ATO_TSNET_GRPC_PIPE")
                .map_err(|_| CapsuleError::Config("Missing ATO_TSNET_GRPC_PIPE".to_string()))?;
            return Ok(IpcTransport::NamedPipe(value));
        }
    }

    pub fn from_endpoint(endpoint: TsnetEndpoint) -> Result<Self> {
        match endpoint {
            #[cfg(unix)]
            TsnetEndpoint::Uds(path) => Ok(IpcTransport::Uds(path)),
            #[cfg(windows)]
            TsnetEndpoint::NamedPipe(pipe) => Ok(IpcTransport::NamedPipe(pipe)),
        }
    }

    pub async fn connect_channel(&self) -> Result<Channel> {
        let endpoint = Endpoint::try_from("http://localhost")
            .map_err(|err| CapsuleError::Config(format!("Invalid gRPC endpoint: {err}")))?;

        match self {
            #[cfg(unix)]
            IpcTransport::Uds(path) => connect_with_uds(endpoint, path.clone()).await,
            #[cfg(windows)]
            IpcTransport::NamedPipe(pipe) => connect_with_pipe(endpoint, pipe.clone()).await,
        }
    }

    pub async fn connect_client(&self) -> Result<TsnetServiceClient<Channel>> {
        let channel = self.connect_channel().await?;
        Ok(TsnetServiceClient::new(channel))
    }
}

#[cfg(unix)]
async fn connect_with_uds(endpoint: Endpoint, path: PathBuf) -> Result<Channel> {
    endpoint
        .connect_with_connector(service_fn(move |_| {
            let path = path.clone();
            async move {
                UnixStream::connect(path)
                    .await
                    .map(TokioIo::new)
                    .map_err(std::io::Error::other)
            }
        }))
        .await
        .map_err(|err| CapsuleError::SidecarIpc(format!("IPC connect failed: {err}")))
}

#[cfg(windows)]
async fn connect_with_pipe(endpoint: Endpoint, pipe: String) -> Result<Channel> {
    use tokio::net::windows::named_pipe::ClientOptions;

    endpoint
        .connect_with_connector(service_fn(move |_| {
            let pipe = pipe.clone();
            async move {
                ClientOptions::new()
                    .open(pipe)
                    .map(TokioIo::new)
                    .map_err(std::io::Error::other)
            }
        }))
        .await
        .map_err(|err| CapsuleError::SidecarIpc(format!("IPC connect failed: {err}")))
}

pub trait IpcIo: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T> IpcIo for T where T: AsyncRead + AsyncWrite + Unpin + Send {}
