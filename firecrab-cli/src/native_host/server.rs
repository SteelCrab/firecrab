//! Native service entry point. Credentials are file references and no plaintext fallback exists.

use clap::Args;
use firecrab_helper_protocol::native_iac::transport::Server;
use firecrab_pki::TlsPaths;
use std::{io, net::SocketAddr, path::PathBuf};
use tokio::{net::TcpListener, task::JoinSet};
use uuid::Uuid;

/// Explicit installation-time transport settings, outside infrastructure manifests/state.
#[derive(Debug, Args)]
pub(crate) struct Options {
    /// Native listener reachable from the management guest; always requires mTLS.
    #[arg(long)]
    bind: SocketAddr,
    /// Persistent API database UUID from `firecrab iac identity`.
    #[arg(long)]
    endpoint_id: Uuid,
    /// Server certificate whose host UUID identifies this native installation.
    #[arg(long)]
    cert: PathBuf,
    /// Native server private key file; its contents never enter protocol frames.
    #[arg(long)]
    key: PathBuf,
    /// Existing Firecrab private CA certificate file.
    #[arg(long)]
    ca: PathBuf,
}

pub(crate) fn run(options: Options) -> io::Result<()> {
    let home = super::selected_home()?;
    let target = super::inspect(&home)?;
    let server = Server::new(
        &TlsPaths {
            cert: options.cert,
            key: options.key,
            ca: options.ca,
        },
        target.installation_id,
        options.endpoint_id,
    )?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let listener = TcpListener::bind(options.bind).await?;
        let mut pending = JoinSet::new();
        loop {
            tokio::select! {
                Some(_) = pending.join_next(), if !pending.is_empty() => (),
                incoming = listener.accept(), if pending.len() < 16 => {
                    let (socket, _) = incoming?;
                    let server = server.clone();
                    let home = home.clone();
                    pending.spawn(async move {
                        let (mut stream, request) = server.accept(socket).await?;
                        // Blocking native locks/file I/O cannot starve TLS admission deadlines.
                        let reply = tokio::task::spawn_blocking(move || super::metadata::handle(&home, request))
                            .await.map_err(|_| io::Error::other("native broker task failed"))??;
                        Server::reply(&mut stream, &reply).await
                    });
                }
            }
        }
    })
}
