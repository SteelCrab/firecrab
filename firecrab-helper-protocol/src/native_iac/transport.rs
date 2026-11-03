//! Bounded native RPC over the existing Firecrab host-certificate mTLS identity.
//!
//! One request uses one connection. A lost connection is an unknown outcome;
//! callers recover the original operation rather than creating a replacement.

use super::{Envelope, Reply, Request, VERSION};
use firecrab_pki::{TlsPaths, host_id, rustls};
use serde::{Serialize, de::DeserializeOwned};
use std::{io, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::{TlsAcceptor, TlsConnector, server::TlsStream};
use uuid::Uuid;

/// Native metadata never needs unbounded configuration or credential payloads.
pub const MAX_FRAME: usize = 16 * 1024;
/// Includes connection, certificate verification and the complete request/reply.
pub const DEADLINE: Duration = Duration::from_secs(10);

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

async fn read<T: DeserializeOwned>(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<T> {
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(invalid("invalid native frame length"));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(|_| invalid("invalid native protocol frame"))
}

async fn write<T: Serialize>(stream: &mut (impl AsyncWrite + Unpin), value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| invalid("invalid native protocol value"))?;
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err(invalid("native protocol value exceeds its frame limit"));
    }
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await
}

/// Client certificate UUID must be this API database's persistent endpoint UUID.
#[derive(Clone)]
pub struct Client {
    address: String,
    server_name: rustls::pki_types::ServerName<'static>,
    connector: TlsConnector,
    endpoint: Uuid,
    installation: Uuid,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeClient")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Preserve CA, validity and hostname checks and also pin the native installation UUID.
    pub fn new(
        address: String,
        server_name: String,
        installation: Uuid,
        endpoint: Uuid,
        paths: &TlsPaths,
    ) -> io::Result<Self> {
        let certificates = firecrab_pki::certificates(&paths.cert)
            .map_err(|_| invalid("native client certificate is unavailable"))?;
        let certificate = certificates
            .first()
            .ok_or_else(|| invalid("native client certificate is missing"))?;
        if endpoint.is_nil()
            || host_id(certificate).ok() != Some(endpoint)
            || address.is_empty()
            || address.len() > 512
        {
            return Err(invalid(
                "native client identity must match the API endpoint",
            ));
        }
        let config = paths
            .client_config_for_host(installation)
            .map_err(|_| invalid("invalid native client TLS references"))?;
        Ok(Self {
            address,
            server_name: server_name
                .try_into()
                .map_err(|_| invalid("invalid native TLS server name"))?,
            connector: TlsConnector::from(Arc::new(config)),
            endpoint,
            installation,
        })
    }

    /// No implicit retry can turn a lost acknowledgement into a new operation.
    pub async fn call(&self, request: Request) -> io::Result<Reply> {
        let envelope = Envelope {
            version: VERSION,
            request,
        };
        envelope.authorize(self.endpoint).map_err(invalid)?;
        tokio::time::timeout(DEADLINE, async {
            let socket = TcpStream::connect(&self.address).await?;
            let mut stream = self
                .connector
                .connect(self.server_name.clone(), socket)
                .await?;
            write(&mut stream, &envelope).await?;
            let reply: Reply = read(&mut stream).await?;
            let target = match &reply {
                Reply::Identity { target, .. } => Some(target),
                Reply::Observation { observation } => Some(&observation.target),
                Reply::OwnerReceipt { receipt } => receipt.as_ref().map(|receipt| &receipt.target),
                Reply::OwnerRejected { rejection } => Some(&rejection.target),
            };
            if target.is_some_and(|target| {
                target.installation_id != self.installation || target.validate().is_err()
            }) {
                return Err(invalid(
                    "native reply does not identify its authenticated installation",
                ));
            }
            Ok(reply)
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "native request outcome is unknown"))?
    }
}

/// Transport identity is checked before a native broker receives any request.
#[derive(Clone)]
pub struct Server {
    acceptor: TlsAcceptor,
    endpoint: Uuid,
}

impl Server {
    /// A CA-signed sibling host cannot exercise this installation's API authority.
    pub fn new(paths: &TlsPaths, installation: Uuid, endpoint: Uuid) -> io::Result<Self> {
        let certificates = firecrab_pki::certificates(&paths.cert)
            .map_err(|_| invalid("native server certificate is unavailable"))?;
        if endpoint.is_nil()
            || installation.is_nil()
            || certificates.first().and_then(|cert| host_id(cert).ok()) != Some(installation)
        {
            return Err(invalid(
                "native server certificate must identify this installation",
            ));
        }
        let config = paths
            .server_config()
            .map_err(|_| invalid("invalid native server TLS references"))?;
        Ok(Self {
            acceptor: TlsAcceptor::from(Arc::new(config)),
            endpoint,
        })
    }

    /// Return only a verified endpoint request; never infer identity from the payload.
    pub async fn accept(&self, socket: TcpStream) -> io::Result<(TlsStream<TcpStream>, Request)> {
        tokio::time::timeout(DEADLINE, async {
            let mut stream = self.acceptor.accept(socket).await?;
            let peer = stream
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|certs| certs.first())
                .and_then(|cert| host_id(cert).ok());
            if peer != Some(self.endpoint) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "native peer does not identify the configured API endpoint",
                ));
            }
            let envelope: Envelope = read(&mut stream).await?;
            envelope.authorize(self.endpoint).map_err(invalid)?;
            Ok((stream, envelope.request))
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "native authentication timed out"))?
    }

    /// A committed result is acknowledged only after its native journal is durable.
    pub async fn reply(stream: &mut TlsStream<TcpStream>, reply: &Reply) -> io::Result<()> {
        tokio::time::timeout(DEADLINE, write(stream, reply))
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "native acknowledgement timed out")
            })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn native_tls_pins_both_installation_and_api_before_dispatch() {
        let root = tempfile::tempdir().unwrap();
        let ca = root.path().join("ca");
        firecrab_pki::init_ca(&ca).unwrap();
        let installation = Uuid::from_u128(42);
        let endpoint = Uuid::from_u128(548);
        let server_paths =
            firecrab_pki::issue_host_cert(&ca, installation, vec!["localhost".into()], 1).unwrap();
        let client_paths = firecrab_pki::issue_host_cert(&ca, endpoint, vec![], 1).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let server = Server::new(&server_paths, installation, endpoint).unwrap();
        let client = Client::new(
            address.clone(),
            "localhost".into(),
            installation,
            endpoint,
            &client_paths,
        )
        .unwrap();
        let (response, ()) = tokio::join!(client.call(Request::Identity), async {
            let (socket, _) = listener.accept().await.unwrap();
            let (mut stream, request) = server.accept(socket).await.unwrap();
            assert_eq!(request, Request::Identity);
            Server::reply(&mut stream, &Reply::OwnerReceipt { receipt: None })
                .await
                .unwrap();
        });
        assert_eq!(response.unwrap(), Reply::OwnerReceipt { receipt: None });
        let sibling = Uuid::from_u128(549);
        let sibling_paths = firecrab_pki::issue_host_cert(&ca, sibling, vec![], 1).unwrap();
        assert!(
            Client::new(
                address.clone(),
                "localhost".into(),
                installation,
                endpoint,
                &sibling_paths
            )
            .is_err()
        );
        let sibling_client = Client::new(
            address.clone(),
            "localhost".into(),
            installation,
            sibling,
            &sibling_paths,
        )
        .unwrap();
        let (response, admitted) = tokio::join!(sibling_client.call(Request::Identity), async {
            let (socket, _) = listener.accept().await.unwrap();
            server.accept(socket).await
        });
        assert!(response.is_err());
        assert!(admitted.is_err());
        let wrong_target = Client::new(
            address.clone(),
            "localhost".into(),
            Uuid::from_u128(43),
            endpoint,
            &client_paths,
        )
        .unwrap();
        let (response, admitted) = tokio::join!(wrong_target.call(Request::Identity), async {
            let (socket, _) = listener.accept().await.unwrap();
            server.accept(socket).await
        });
        assert!(response.is_err());
        assert!(admitted.is_err());
        assert!(
            client
                .call(Request::OwnerReceipt {
                    operation_id: Uuid::from_u128(1),
                    endpoint_id: sibling
                })
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn oversized_or_truncated_frames_are_rejected_before_deserialization() {
        let (mut sender, mut receiver) = tokio::io::duplex(32);
        sender.write_u32((MAX_FRAME + 1) as u32).await.unwrap();
        assert!(read::<Envelope>(&mut receiver).await.is_err());
        sender.write_u32(10).await.unwrap();
        sender.write_all(b"{}").await.unwrap();
        drop(sender);
        assert!(read::<Envelope>(&mut receiver).await.is_err());
    }
}
