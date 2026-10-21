//! Public subdomain registration and forwarding service. TLS is terminated by the operator ingress.
use clap::Parser;
use std::{net::SocketAddr, path::PathBuf};

/// Operator settings for the listener, durable registry and trusted upstreams.
#[derive(Parser)]
struct Options {
    /// Read CF-Connecting-IP only from a local cloudflared connector.
    #[arg(long, env = "FIRECRAB_TUNNEL_TRUST_CLOUDFLARE_PROXY")]
    trust_cloudflare_proxy: bool,
    /// Socket address on which the service accepts HTTP requests.
    #[arg(long, env = "FIRECRAB_TUNNEL_LISTEN", default_value = "127.0.0.1:8790")]
    listen: SocketAddr,
    /// SQLite file that retains address ownership across restarts.
    #[arg(
        long,
        env = "FIRECRAB_TUNNEL_DATABASE",
        default_value = "firecrab-tunnel.db"
    )]
    database: PathBuf,
    /// Public DNS suffix served by this registry.
    #[arg(long, env = "FIRECRAB_TUNNEL_DOMAIN", default_value = "firecrab.dev")]
    domain: String,
    /// Explicitly trusted direct Nginx origins; otherwise only HTTPS Quick Tunnel origins are accepted.
    #[arg(long, env = "FIRECRAB_TUNNEL_NGINX_ORIGINS", value_delimiter = ',')]
    nginx_origin: Vec<String>,
}

/// Start the registry and proxy, then drain the server on Ctrl-C.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Options::parse();
    let mut service =
        firecrab_tunnel::Service::open(&args.database, args.domain, args.nginx_origin)?;
    if args.trust_cloudflare_proxy {
        if !args.listen.ip().is_loopback() {
            return Err("trusted Cloudflare proxy requires a loopback listener".into());
        }
        service = service.with_cloudflare_proxy();
    }
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    eprintln!(
        "Firecrab connection service listening on {}",
        listener.local_addr()?
    );
    axum::serve(
        listener,
        service
            .router()
            .into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}
