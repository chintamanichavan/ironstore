use std::sync::Arc;

use bytes::BytesMut;
use pin_project::pin_project;
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::error::{Result, VektaError};
use crate::network::protocol::{
    decode_response, decode_search_results, encode_request, Request, Response,
};

#[pin_project(project = TransportProj)]
enum Transport {
    Plain(#[pin] TcpStream),
    Tls(#[pin] tokio_rustls::client::TlsStream<TcpStream>),
}

impl AsyncRead for Transport {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.project() {
            TransportProj::Plain(s) => s.poll_read(cx, buf),
            TransportProj::Tls(s) => s.poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Transport {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.project() {
            TransportProj::Plain(s) => s.poll_write(cx, buf),
            TransportProj::Tls(s) => s.poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.project() {
            TransportProj::Plain(s) => s.poll_flush(cx),
            TransportProj::Tls(s) => s.poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.project() {
            TransportProj::Plain(s) => s.poll_shutdown(cx),
            TransportProj::Tls(s) => s.poll_shutdown(cx),
        }
    }
}

pub struct Client {
    stream: Transport,
    buf: BytesMut,
}

impl Client {
    pub async fn connect(addr: &str) -> Result<Self> {
        let stream = TcpStream::connect(addr)
            .await
            .map_err(|e| VektaError::Network(format!("connect failed: {e}")))?;
        Ok(Self {
            stream: Transport::Plain(stream),
            buf: BytesMut::with_capacity(4096),
        })
    }

    pub async fn connect_tls(addr: &str, tls_config: Arc<ClientConfig>) -> Result<Self> {
        let tcp = TcpStream::connect(addr)
            .await
            .map_err(|e| VektaError::Network(format!("connect failed: {e}")))?;
        let host = addr.split(':').next().unwrap_or("localhost");
        let server_name: ServerName<'static> = ServerName::try_from(host.to_string())
            .map_err(|e| VektaError::Network(format!("invalid server name: {e}")))?;
        let connector = TlsConnector::from(tls_config);
        let tls = connector
            .connect(server_name, tcp)
            .await
            .map_err(|e| VektaError::Network(format!("TLS handshake failed: {e}")))?;
        Ok(Self {
            stream: Transport::Tls(tls),
            buf: BytesMut::with_capacity(4096),
        })
    }

    async fn send_recv(&mut self, req: &Request) -> Result<Response> {
        let frame = encode_request(req);
        self.stream.write_all(&frame).await?;
        loop {
            if let Some(resp) = decode_response(&mut self.buf) {
                return Ok(resp);
            }
            let n = self.stream.read_buf(&mut self.buf).await?;
            if n == 0 {
                return Err(VektaError::Network("server closed connection".into()));
            }
        }
    }

    pub async fn get(&mut self, key: &str) -> Result<Option<String>> {
        let req = Request::Get {
            key: key.as_bytes().to_vec().into(),
        };
        match self.send_recv(&req).await? {
            Response::Ok(v) => Ok(Some(String::from_utf8_lossy(&v).into_owned())),
            Response::NotFound => Ok(None),
            Response::Error(e) => Err(VektaError::Network(e)),
        }
    }

    pub async fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let req = Request::Set {
            key: key.as_bytes().to_vec().into(),
            value: value.as_bytes().to_vec().into(),
        };
        match self.send_recv(&req).await? {
            Response::Ok(_) => Ok(()),
            Response::Error(e) => Err(VektaError::Network(e)),
            Response::NotFound => Err(VektaError::Network("unexpected NOT_FOUND on SET".into())),
        }
    }

    pub async fn delete(&mut self, key: &str) -> Result<()> {
        let req = Request::Delete {
            key: key.as_bytes().to_vec().into(),
        };
        match self.send_recv(&req).await? {
            Response::Ok(_) | Response::NotFound => Ok(()),
            Response::Error(e) => Err(VektaError::Network(e)),
        }
    }

    pub async fn stats(&mut self) -> Result<String> {
        match self.send_recv(&Request::Stats).await? {
            Response::Ok(v) => Ok(String::from_utf8_lossy(&v).into_owned()),
            Response::Error(e) => Err(VektaError::Network(e)),
            Response::NotFound => Ok(String::new()),
        }
    }

    pub async fn auth(&mut self, token: &str) -> Result<()> {
        let req = Request::Auth {
            token: token.as_bytes().to_vec().into(),
        };
        match self.send_recv(&req).await? {
            Response::Ok(_) => Ok(()),
            Response::Error(e) => Err(VektaError::Network(e)),
            Response::NotFound => Err(VektaError::Network("unexpected NOT_FOUND on AUTH".into())),
        }
    }

    pub async fn set_vec(&mut self, key: &str, vector: Vec<f32>) -> Result<()> {
        let req = Request::SetVec {
            key: key.as_bytes().to_vec().into(),
            vector,
        };
        match self.send_recv(&req).await? {
            Response::Ok(_) => Ok(()),
            Response::Error(e) => Err(VektaError::Network(e)),
            Response::NotFound => Err(VektaError::Network("unexpected NOT_FOUND on SET_VEC".into())),
        }
    }

    pub async fn search(&mut self, query: Vec<f32>, k: u32) -> Result<Vec<(String, f32)>> {
        let req = Request::Search { k, query };
        match self.send_recv(&req).await? {
            Response::Ok(payload) => Ok(decode_search_results(&payload)),
            Response::Error(e) => Err(VektaError::Network(e)),
            Response::NotFound => Ok(vec![]),
        }
    }
}
