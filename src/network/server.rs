use std::sync::Arc;

use bytes::BytesMut;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio_rustls::TlsAcceptor;

use crate::error::Result;
use crate::network::protocol::{
    decode_request, encode_response, encode_search_results, Request, Response,
};
use crate::storage::engine::Engine;

pub async fn run_server(
    addr: &str,
    engine: Arc<RwLock<Engine>>,
    auth_token: Option<String>,
    tls: Option<Arc<rustls::ServerConfig>>,
) -> Result<()> {
    let listener = TcpListener::bind(addr).await.map_err(|e| {
        crate::error::VektaError::Network(format!("bind failed: {e}"))
    })?;
    tracing::info!(
        "VektaLite listening on {addr}{}{}",
        if tls.is_some() { " (TLS)" } else { "" },
        if auth_token.is_some() { " (auth enabled)" } else { "" },
    );

    let auth_token = Arc::new(auth_token);
    let acceptor = tls.map(TlsAcceptor::from);

    loop {
        let (stream, peer) = listener.accept().await.map_err(|e| {
            crate::error::VektaError::Network(format!("accept failed: {e}"))
        })?;
        tracing::debug!("New connection from {peer}");
        let engine = Arc::clone(&engine);
        let token = Arc::clone(&auth_token);
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            match &acceptor {
                None => {
                    if let Err(e) = handle_connection(stream, engine, token).await {
                        tracing::warn!("Connection {peer} error: {e}");
                    }
                }
                Some(acc) => match acc.accept(stream).await {
                    Ok(tls_stream) => {
                        if let Err(e) = handle_connection(tls_stream, engine, token).await {
                            tracing::warn!("Connection {peer} error: {e}");
                        }
                    }
                    Err(e) => tracing::warn!("TLS handshake {peer} failed: {e}"),
                },
            }
        });
    }
}

async fn handle_connection<S>(
    mut stream: S,
    engine: Arc<RwLock<Engine>>,
    auth_token: Arc<Option<String>>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let mut buf = BytesMut::with_capacity(4096);
    let mut authenticated = auth_token.is_none();

    loop {
        let n = stream.read_buf(&mut buf).await?;
        if n == 0 {
            break;
        }
        while let Some(req) = decode_request(&mut buf) {
            let resp = if !authenticated {
                match req {
                    Request::Auth { ref token } => {
                        if auth_token.as_deref() == Some(std::str::from_utf8(token).unwrap_or("")) {
                            authenticated = true;
                            Response::Ok(bytes::Bytes::from_static(b"OK"))
                        } else {
                            let encoded = encode_response(&Response::Error("AUTH failed".into()));
                            stream.write_all(&encoded).await?;
                            return Ok(());
                        }
                    }
                    _ => Response::Error("NOAUTH: send AUTH token first".into()),
                }
            } else {
                dispatch(req, &engine).await
            };
            let encoded = encode_response(&resp);
            stream.write_all(&encoded).await?;
        }
    }
    Ok(())
}

async fn dispatch(req: Request, engine: &Arc<RwLock<Engine>>) -> Response {
    match req {
        Request::Get { key } => {
            let guard = engine.read().await;
            match guard.get(&key) {
                Ok(Some(v)) => Response::Ok(v.into()),
                Ok(None) => Response::NotFound,
                Err(e) => Response::Error(e.to_string()),
            }
        }
        Request::Set { key, value } => {
            let mut guard = engine.write().await;
            match guard.set(key.to_vec(), value.to_vec()) {
                Ok(_) => Response::Ok(bytes::Bytes::from_static(b"OK")),
                Err(e) => Response::Error(e.to_string()),
            }
        }
        Request::Delete { key } => {
            let mut guard = engine.write().await;
            match guard.delete(&key) {
                Ok(_) => Response::Ok(bytes::Bytes::from_static(b"OK")),
                Err(e) => Response::Error(e.to_string()),
            }
        }
        Request::Stats => {
            let guard = engine.read().await;
            let s = guard.stats();
            let max_mem = if s.max_memory_bytes == usize::MAX {
                "unlimited".to_string()
            } else {
                format!("{}MB", s.max_memory_bytes / 1024 / 1024)
            };
            let msg = format!(
                "memtable_size={} memtable_entries={} sstables={} sstable_keys={} vectors={} memory={}B limit={}",
                s.memtable_size_bytes,
                s.memtable_entries,
                s.sstable_count,
                s.sstable_entries,
                s.vector_count,
                s.memory_bytes,
                max_mem,
            );
            Response::Ok(msg.into_bytes().into())
        }
        Request::SetVec { key, vector } => {
            let key_str = String::from_utf8_lossy(&key).into_owned();
            let mut guard = engine.write().await;
            match guard.set_vec(key_str, vector) {
                Ok(_) => Response::Ok(bytes::Bytes::from_static(b"OK")),
                Err(e) => Response::Error(e.to_string()),
            }
        }
        Request::Search { k, query } => {
            let guard = engine.read().await;
            match guard.search(&query, k as usize) {
                Ok(results) => Response::Ok(encode_search_results(&results)),
                Err(e) => Response::Error(e.to_string()),
            }
        }
        Request::Auth { .. } => {
            Response::Ok(bytes::Bytes::from_static(b"OK"))
        }
    }
}
