use std::{
    io,
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};

use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt, copy_bidirectional};

use crate::{
    gateway::dataplane::DataPlaneRuntime,
    socket::{
        tcp::{TcpListenOptions, VirtualTcpListener, VirtualTcpListenerFactory, VirtualTcpSocketFactory},
        udp::VirtualUdpSocketFactory,
    },
};

/// On-device HTTP/HTTPS proxy portal.
///
/// Listens on a local address (typically `127.0.0.1`) for traffic that the
/// Android VPN `setHttpProxy` redirects there, then forwards each connection to
/// the configured upstream HTTP proxy through the VPN data plane. It is
/// independent of the exit-node proxy and only covers HTTP/HTTPS (ports 80/443),
/// which is exactly what `setHttpProxy` steers.
///
/// The upstream connection is opened through the core's smoltcp data plane so it
/// is tunneled to the VPN-internal proxy address. A plain host socket cannot
/// reach a VPN-internal IP because the app is excluded from its own VPN.
pub struct HttpProxyPortal<H>
where
    H: VirtualTcpSocketFactory + VirtualTcpListenerFactory + VirtualUdpSocketFactory,
{
    upstream: SocketAddr,
    local_addr: SocketAddr,
    data_plane: Arc<DataPlaneRuntime<H>>,
}

impl<H> HttpProxyPortal<H>
where
    H: VirtualTcpSocketFactory + VirtualTcpListenerFactory + VirtualUdpSocketFactory,
{
    pub fn new(
        upstream: SocketAddr,
        local_addr: SocketAddr,
        data_plane: Arc<DataPlaneRuntime<H>>,
    ) -> Self {
        Self {
            upstream,
            local_addr,
            data_plane,
        }
    }

    /// Bind the local listener and serve until the listener errors.
    pub async fn start(self: Arc<Self>) -> Result<u16, io::Error> {
        let options = TcpListenOptions::manual_connect(self.local_addr);
        let listener = self
            .data_plane
            .host
            .bind_tcp(options)
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let local_addr = listener.local_addr()?;
        let port = local_addr.port();

        let this = self.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((client, peer)) => {
                        let this = this.clone();
                        tokio::spawn(async move {
                            if let Err(e) = this.handle(client).await {
                                tracing::debug!(?e, %peer, "http proxy portal connection error");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!(?e, "http proxy portal accept failed, stopping listener");
                        break;
                    }
                }
            }
        });

        Ok(port)
    }

    async fn handle(
        self: Arc<Self>,
        mut client: <H::Listener as VirtualTcpListener>::Socket,
    ) -> Result<()> {
        let request = read_until_double_crlf(&mut client).await?;
        if request.is_empty() {
            return Ok(());
        }

        let mut upstream = self
            .data_plane
            .data_plane_tcp_connect(self.upstream, Duration::from_secs(30))
            .await
            .map_err(|e| anyhow::anyhow!("data plane connect to upstream proxy failed: {e}"))?;

        // Forward the client's request verbatim to the upstream proxy: for a
        // CONNECT it carries the real target, for an absolute-form GET it is
        // the full proxy request.
        upstream.write_all(&request).await?;
        upstream.flush().await?;

        let response = read_until_double_crlf(&mut upstream).await?;
        client.write_all(&response).await?;
        client.flush().await?;

        // If the upstream rejected the request, stop here: the error response
        // has already been forwarded to the client.
        if !response_status_is_success(&response) {
            return Ok(());
        }

        let (down, up) = copy_bidirectional(&mut client, &mut upstream).await?;
        tracing::trace!(down, up, "http proxy portal session closed");
        Ok(())
    }
}

fn response_status_is_success(response: &[u8]) -> bool {
    let text = String::from_utf8_lossy(response);
    let Some(status_line) = text.split("\r\n").next() else {
        return false;
    };
    let mut parts = status_line.split_whitespace();
    let Some(code) = parts.nth(1) else {
        return false;
    };
    matches!(code, "200" | "201" | "202" | "203" | "204" | "205" | "206")
}

async fn read_until_double_crlf<S>(stream: &mut S) -> io::Result<Vec<u8>>
where
    S: AsyncReadExt + Unpin,
{
    let mut buf = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    let mut last_was_cr = false;
    let mut crlf_count = 0;
    loop {
        let n = stream.read(&mut byte).await?;
        if n == 0 {
            return Ok(buf);
        }
        buf.push(byte[0]);
        if byte[0] == b'\r' {
            last_was_cr = true;
        } else if byte[0] == b'\n' {
            if last_was_cr {
                crlf_count += 1;
            } else {
                crlf_count = 0;
            }
            last_was_cr = false;
            if crlf_count >= 2 {
                break;
            }
        } else {
            last_was_cr = false;
            crlf_count = 0;
        }
        if buf.len() > 64 * 1024 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "http request too large"));
        }
    }
    Ok(buf)
}

/// Resolve the local listen address for the portal. A missing/empty portal
/// address defaults to `127.0.0.1:7890`, which is what Android's `setHttpProxy`
/// must target.
pub fn resolve_local_addr(portal: Option<SocketAddr>) -> SocketAddr {
    portal.unwrap_or_else(|| "127.0.0.1:7890".parse().expect("valid default listen addr"))
}

impl<H> std::fmt::Debug for HttpProxyPortal<H>
where
    H: VirtualTcpSocketFactory + VirtualTcpListenerFactory + VirtualUdpSocketFactory,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpProxyPortal")
            .field("upstream", &self.upstream)
            .field("local_addr", &self.local_addr)
            .finish_non_exhaustive()
    }
}
