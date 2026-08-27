use std::net::SocketAddr;

use anyhow::Context;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use super::{tcp_proxy_engine::TcpProxyMode, traits::TcpProxyDestinationConnector};

/// A [`TcpProxyDestinationConnector`] that tunnels connections through an
/// upstream HTTP CONNECT proxy.
///
/// Sends a `CONNECT dst_ip:dst_port HTTP/1.1` request to the proxy and
/// waits for a `200` response before forwarding data bidirectionally.
/// Works for both HTTP (80) and HTTPS (443) traffic.
pub struct HttpProxyConnector {
    proxy_addr: SocketAddr,
}

impl HttpProxyConnector {
    pub fn new(proxy_addr: SocketAddr) -> Self {
        Self { proxy_addr }
    }

    /// Send CONNECT request to the proxy and wait for 200 response.
    async fn connect_through_proxy(&self, dst: SocketAddr) -> anyhow::Result<TcpStream> {
        let mut stream = TcpStream::connect(self.proxy_addr)
            .await
            .with_context(|| format!("connect to HTTP proxy {}", self.proxy_addr))?;

        let connect_req = format!("CONNECT {}:{} HTTP/1.1\r\n\r\n", dst.ip(), dst.port());
        stream
            .write_all(connect_req.as_bytes())
            .await
            .context("send CONNECT request")?;

        // Read the status line
        let mut reader = BufReader::new(&mut stream);
        let mut status_line = String::new();
        reader
            .read_line(&mut status_line)
            .await
            .context("read CONNECT response status")?;

        // Consume remaining headers until empty line
        loop {
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .await
                .context("read CONNECT header")?;
            if line.trim().is_empty() {
                break;
            }
        }

        if !status_line.contains("200") {
            anyhow::bail!(
                "HTTP CONNECT to {}:{} failed: {}",
                dst.ip(),
                dst.port(),
                status_line.trim()
            );
        }

        // Get back the underlying stream ( BufReader drop releases the borrow )
        drop(reader);
        Ok(stream)
    }
}

#[async_trait::async_trait]
impl TcpProxyDestinationConnector for HttpProxyConnector {
    type DstStream = TcpStream;

    async fn connect(&self, _src: SocketAddr, dst: SocketAddr) -> anyhow::Result<Self::DstStream> {
        self.connect_through_proxy(dst).await
    }

    fn proxy_mode(&self) -> TcpProxyMode {
        TcpProxyMode::Tcp
    }
}
