use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;

use vektalite::error::Result;
use vektalite::network::server::run_server;
use vektalite::storage::engine::Engine;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("vektalite=info".parse().unwrap()),
        )
        .init();

    let data_dir = std::env::var("VEKTALITE_DATA").unwrap_or_else(|_| "./data".into());
    let addr = std::env::var("VEKTALITE_ADDR").unwrap_or_else(|_| "127.0.0.1:6380".into());
    let auth_token = std::env::var("VEKTALITE_AUTH").ok();

    // TLS: set VEKTALITE_TLS_CERT + VEKTALITE_TLS_KEY to enable.
    let tls_config = match (
        std::env::var("VEKTALITE_TLS_CERT").ok(),
        std::env::var("VEKTALITE_TLS_KEY").ok(),
    ) {
        (Some(cert), Some(key)) => Some(vektalite::tls::server_config(cert, key)?),
        (None, None) => None,
        _ => {
            return Err(vektalite::error::VektaError::Network(
                "TLS: set both VEKTALITE_TLS_CERT and VEKTALITE_TLS_KEY".into(),
            ))
        }
    };

    let engine = Engine::open(&data_dir)?;
    let shared = Arc::new(RwLock::new(engine));

    // Background compaction: checks every 30s so flushes never stall on compaction.
    let compaction_handle = Arc::clone(&shared);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        interval.tick().await; // skip the immediate first tick
        loop {
            interval.tick().await;
            let mut guard = compaction_handle.write().await;
            match guard.maybe_compact() {
                Ok(true) => tracing::info!("Background compaction completed"),
                Ok(false) => {}
                Err(e) => tracing::error!("Compaction error: {e}"),
            }
        }
    });

    // Graceful shutdown on SIGTERM or Ctrl-C: flush MemTable before exit.
    let shutdown_handle = Arc::clone(&shared);
    let server_handle = Arc::clone(&shared);

    tokio::select! {
        result = run_server(&addr, server_handle, auth_token, tls_config) => result,
        _ = shutdown_signal() => {
            tracing::info!("Shutdown signal received, flushing MemTable…");
            shutdown_handle.write().await.flush().ok();
            tracing::info!("Shutdown complete");
            Ok(())
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async { tokio::signal::ctrl_c().await.ok() };

    #[cfg(unix)]
    let sigterm = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await
    };

    #[cfg(not(unix))]
    let sigterm = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = sigterm => {}
    }
}
