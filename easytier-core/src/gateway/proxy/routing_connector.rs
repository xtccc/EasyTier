use std::net::SocketAddr;

use crate::socket::tcp::VirtualTcpSocketFactory;

use super::{
    http_proxy_connector::HttpProxyConnector,
    tcp_proxy_engine::TcpProxyMode,
    tcp_socket_connector::TcpSocketProxyConnector,
    traits::{TcpProxyDestinationConnector, TcpProxyStream},
};

/// Routes TCP connections either through an HTTP proxy (for ports 80 and 443)
/// or directly to the destination via the regular socket connector.
pub(crate) struct RoutingTcpProxyConnector<F: VirtualTcpSocketFactory> {
    direct: TcpSocketProxyConnector<F>,
    http_proxy: Option<HttpProxyConnector>,
}

impl<F: VirtualTcpSocketFactory> RoutingTcpProxyConnector<F> {
    pub fn new(direct: TcpSocketProxyConnector<F>, http_proxy: Option<SocketAddr>) -> Self {
        Self {
            direct,
            http_proxy: http_proxy.map(HttpProxyConnector::new),
        }
    }
}

#[async_trait::async_trait]
impl<F: VirtualTcpSocketFactory> TcpProxyDestinationConnector
    for RoutingTcpProxyConnector<F>
{
    type DstStream = Box<dyn TcpProxyStream>;

    async fn connect(&self, src: SocketAddr, dst: SocketAddr) -> anyhow::Result<Self::DstStream> {
        // Route through HTTP proxy for ports 80 and 443 when proxy is configured
        if let Some(ref http_proxy) = self.http_proxy {
            if dst.port() == 80 || dst.port() == 443 {
                let stream = http_proxy.connect(src, dst).await?;
                return Ok(Box::new(stream));
            }
        }
        // Default: direct connection
        let stream = self.direct.connect(src, dst).await?;
        Ok(Box::new(stream))
    }

    fn proxy_mode(&self) -> TcpProxyMode {
        TcpProxyMode::Tcp
    }
}
