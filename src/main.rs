//! pstd-bridge: the person's own mailbox between their Claude and pstd. CLAUDE_BRIDGE.md is the spec.

mod api;
mod images;
mod mcp;
mod review;
mod setup;
mod store;
#[cfg(test)]
mod tests;

use api::Bridge;
use futures::StreamExt;
use rustls_acme::caches::DirCache;
use rustls_acme::AcmeConfig;
use std::path::PathBuf;
use std::sync::Arc;
use store::Store;

const USAGE: &str = "pstd-bridge setup     set this server up, step by step
pstd-bridge serve     run the server
pstd-bridge connect   show the QR for pstd and the line for Claude again
pstd-bridge key       set the image generator's key
pstd-bridge review    open a test mailbox for App Review and show its link; `review off` closes it";

pub const DEFAULT_BIND: &str = "127.0.0.1:8787";
const HTTPS_PORT: u16 = 443;

pub fn home() -> PathBuf {
    if let Some(path) = std::env::var_os("PSTD_BRIDGE_HOME") {
        return path.into();
    }
    if setup::is_root() {
        return "/var/lib/pstd-bridge".into();
    }
    let user = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).unwrap_or_else(|| ".".into());
    PathBuf::from(user).join(".pstd-bridge")
}

async fn serve(store: Arc<Store>) -> std::io::Result<()> {
    let config = store.config()?;
    // at the start too: a file left from before a restart shouldn't wait for the next upload
    let sweeping = store.clone();
    tokio::spawn(async move {
        let mut hourly = tokio::time::interval(std::time::Duration::from_secs(60 * 60));
        loop {
            hourly.tick().await;
            sweeping.sweep();
        }
    });
    let app = api::router(Bridge { images: images::openai(store.clone()), store: store.clone() });
    if config.tls_host.is_empty() {
        let bind = if config.bind.is_empty() { DEFAULT_BIND } else { &config.bind };
        println!("listening on http://{bind}");
        return axum::serve(tokio::net::TcpListener::bind(bind).await?, app).await;
    }
    // the certificate comes and renews by itself, over the port it serves on
    let mut acme = AcmeConfig::new([config.tls_host.clone()])
        .cache(DirCache::new(store.root().join("certificates")))
        .directory_lets_encrypt(true)
        .state();
    let acceptor = acme.axum_acceptor(acme.default_rustls_config());
    tokio::spawn(async move {
        while let Some(event) = acme.next().await {
            match event {
                Ok(ok) => println!("certificate: {ok:?}"),
                Err(error) => eprintln!("certificate: {error}"),
            }
        }
    });
    println!("listening on https://{}", config.tls_host);
    axum_server::bind(std::net::SocketAddr::from(([0, 0, 0, 0], HTTPS_PORT))).acceptor(acceptor).serve(app.into_make_service()).await
}

#[tokio::main]
async fn main() {
    let command = std::env::args().nth(1).unwrap_or_default();
    let result = async {
        let store = Arc::new(Store::open(home())?);
        match command.as_str() {
            "serve" => serve(store).await,
            "setup" => {
                let args: Vec<String> = std::env::args().skip(2).collect();
                let domain = args.iter().position(|a| a == "--domain").and_then(|i| args.get(i + 1)).cloned();
                setup::run(&store, domain, args.iter().any(|a| a == "--auto")).await
            }
            "connect" => setup::connect(&store),
            "key" => setup::key(&store),
            "review" => setup::review(&store, std::env::args().nth(2).as_deref() == Some("off")),
            _ => {
                println!("{USAGE}");
                Ok(())
            }
        }
    }
    .await;
    if let Err(error) = result {
        eprintln!("pstd-bridge: {error}");
        std::process::exit(1);
    }
}
