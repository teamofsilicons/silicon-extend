//! TCP and TLS connections to devices on the local network.
//!
//! TVs serve their secure sockets with self-signed certificates that no CA vouches for, so the
//! connections to them skip certificate checks: the only protection available is being on the same
//! network, and the pairing token or client key is what authorises us. Downloads from the internet
//! (`display show --image https://…`) use normal verification.
//!
//! TLS is the platform's own (Secure Transport, SChannel, OpenSSL) through `native-tls`, which
//! keeps C crypto builds out of cross-compiles.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_native_tls::TlsStream;

/// A plain or TLS stream.
pub(crate) enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

pub(crate) async fn tcp(host: &str, port: u16, timeout: Duration) -> io::Result<TcpStream> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let s = tokio::time::timeout(timeout, TcpStream::connect((host, port)))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, format!("{host}:{port} didn't answer")))??;
    let _ = s.set_nodelay(true);
    Ok(s)
}

/// Connects with TLS, accepting the self-signed certificates TVs use.
pub(crate) async fn tls_insecure(host: &str, port: u16, timeout: Duration) -> io::Result<Stream> {
    let connector = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .min_protocol_version(Some(native_tls::Protocol::Tlsv10))
        .build()
        .map_err(io::Error::other)?;
    tls_with(connector, host, port, timeout).await
}

/// Connects with TLS and normal certificate verification.
pub(crate) async fn tls_verified(host: &str, port: u16, timeout: Duration) -> io::Result<Stream> {
    let connector = native_tls::TlsConnector::new().map_err(io::Error::other)?;
    tls_with(connector, host, port, timeout).await
}

async fn tls_with(connector: native_tls::TlsConnector, host: &str, port: u16, timeout: Duration) -> io::Result<Stream> {
    let tcp = tcp(host, port, timeout).await?;
    let connector = tokio_native_tls::TlsConnector::from(connector);
    let domain = host.trim_start_matches('[').trim_end_matches(']');
    let tls = tokio::time::timeout(timeout, connector.connect(domain, tcp))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake timed out"))?
        .map_err(io::Error::other)?;
    Ok(Stream::Tls(Box::new(tls)))
}

impl AsyncRead for Stream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_flush(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}
