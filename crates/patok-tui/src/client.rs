//! gRPC client over the engine's Unix socket.

use std::path::{Path, PathBuf};

use hyper_util::rt::TokioIo;
use patok_proto::engine_client::EngineClient;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

/// Connects to the engine socket. Fails fast when there is no socket or nobody is listening.
pub async fn connect(socket: &Path) -> Result<EngineClient<Channel>, tonic::transport::Error> {
    let socket: PathBuf = socket.to_path_buf();
    // The URI is required by the API but never used: the connector dials the socket.
    let channel = Endpoint::try_from("http://engine.invalid")?
        .connect_with_connector(service_fn(move |_: Uri| {
            let socket = socket.clone();
            async move { Ok::<_, std::io::Error>(TokioIo::new(UnixStream::connect(socket).await?)) }
        }))
        .await?;
    Ok(EngineClient::new(channel))
}
