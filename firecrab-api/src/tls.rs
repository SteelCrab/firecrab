//! The remote API accepts only CA-authenticated, UUID-identified peers.

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::extract::connect_info::Connected;
use axum::serve::{IncomingStream, Listener};
use firecrab_pki::{Error, TlsPaths, host_id, rustls::ServerConfig};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio_rustls::{TlsAcceptor, server::TlsStream};
use uuid::Uuid;

const MAX_HANDSHAKES: usize = 64;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

pub fn configuration(
    bind: SocketAddr,
    paths: Option<TlsPaths>,
) -> Result<Option<Arc<ServerConfig>>, Error> {
    match paths {
        Some(paths) => paths.server_config().map(Arc::new).map(Some),
        None if bind.ip().is_loopback() => Ok(None),
        None => Err(Error("non-loopback bind requires FIRECRAB_TLS_CERT, FIRECRAB_TLS_KEY, and FIRECRAB_TLS_CA; boolean authentication/TLS flags do not enable security".into())),
    }
}

/// Identity comes from the verified TLS connection, never an HTTP header/body.
#[derive(Clone, Copy, Debug)]
pub struct AuthenticatedHost {
    pub host_id: Uuid,
}

pub async fn serve(
    listener: TcpListener,
    app: Router,
    config: Option<Arc<ServerConfig>>,
) -> io::Result<()> {
    if config.is_none() && !listener.local_addr()?.ip().is_loopback() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "plaintext API listeners must bind to loopback",
        ));
    }
    match config {
        None => axum::serve(listener, app).await,
        Some(config) => {
            let listener = TlsListener {
                listener,
                acceptor: TlsAcceptor::from(config),
                handshakes: JoinSet::new(),
            };
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<AuthenticatedHost>(),
            )
            .await
        }
    }
}

struct AuthenticatedStream {
    stream: TlsStream<TcpStream>,
    host_id: Uuid,
}

impl AsyncRead for AuthenticatedStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buffer)
    }
}

impl AsyncWrite for AuthenticatedStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buffer)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

struct TlsListener {
    listener: TcpListener,
    acceptor: TlsAcceptor,
    handshakes: JoinSet<Option<(AuthenticatedStream, SocketAddr)>>,
}

impl Connected<IncomingStream<'_, TlsListener>> for AuthenticatedHost {
    fn connect_info(stream: IncomingStream<'_, TlsListener>) -> Self {
        let peer = Self {
            host_id: stream.io().host_id,
        };
        tracing::debug!(host_id = %peer.host_id, "authenticated remote API peer");
        peer
    }
}

impl Listener for TlsListener {
    type Io = AuthenticatedStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            tokio::select! {
                Some(result) = self.handshakes.join_next(), if !self.handshakes.is_empty() => {
                    if let Ok(Some(connection)) = result { return connection; }
                }
                incoming = self.listener.accept(), if self.handshakes.len() < MAX_HANDSHAKES => {
                    match incoming {
                        Ok((socket, address)) => {
                            let acceptor = self.acceptor.clone();
                            self.handshakes.spawn(async move {
                                let stream = match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(socket)).await {
                                    Ok(Ok(stream)) => stream,
                                    _ => { tracing::debug!(%address, "rejected or timed out API TLS handshake"); return None; }
                                };
                                let certificate = stream.get_ref().1.peer_certificates()?.first()?;
                                let id = host_id(certificate).ok()?;
                                Some((AuthenticatedStream {stream, host_id: id}, address))
                            });
                        }
                        Err(error) => {
                            tracing::warn!(%error, "API TLS accept failed");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

#[cfg(test)]
mod tests;
