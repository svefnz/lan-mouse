//! TLS-over-TCP channel carrying clipboard frames.
//!
//! Binds the same port number as the input protocol: TCP and UDP port
//! namespaces are independent, so no extra configuration key is needed
//! and the input path is completely unaffected.

use std::{
    cell::RefCell,
    collections::HashMap,
    io,
    net::SocketAddr,
    rc::Rc,
    sync::{Arc, RwLock},
};

use input_clipboard::Representation;
use rustls::pki_types::ServerName;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_rustls::{
    TlsAcceptor, TlsConnector, client::TlsStream as ClientStream, server::TlsStream as ServerStream,
};
use webrtc_dtls::crypto::Certificate;

use crate::clipboard::{
    frame::{self, DecodeError, Frame},
    tls,
};

/// Server name presented by the client. Every lan-mouse certificate is
/// self-signed, so the name is a placeholder that both sides ignore.
const SERVER_NAME: &str = "lan-mouse";

#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Tls(#[from] rustls::Error),
    #[error(transparent)]
    Frame(#[from] DecodeError),
    #[error("invalid server name")]
    ServerName,
}

/// Accepts clipboard connections from authorized peers.
pub struct Server {
    listener: TcpListener,
    acceptor: TlsAcceptor,
}

impl Server {
    pub async fn bind(
        port: u16,
        cert: &Certificate,
        authorized: Arc<RwLock<HashMap<String, String>>>,
    ) -> Result<Self, ChannelError> {
        let config = tls::server_config(cert, authorized)?;
        let listener = TcpListener::bind(("0.0.0.0", port)).await?;
        log::debug!("clipboard: listening on tcp {port}");
        Ok(Self {
            listener,
            acceptor: TlsAcceptor::from(Arc::new(config)),
        })
    }

    /// The address actually bound, useful when binding port 0.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Accept one authenticated peer.
    pub async fn accept(&self) -> Result<(SocketAddr, ServerStream<TcpStream>), ChannelError> {
        let (stream, addr) = self.listener.accept().await?;
        let stream = self.acceptor.accept(stream).await?;
        log::debug!("clipboard: peer connected from {addr}");
        Ok((addr, stream))
    }
}

/// Sends clipboard frames to peers, keeping connections warm.
///
/// Connections are opened on first use and cached, so a later transfer to
/// the same peer costs no handshake.
pub struct Client {
    config: Arc<rustls::ClientConfig>,
    conns: RefCell<HashMap<SocketAddr, ClientStream<TcpStream>>>,
}

impl Client {
    pub fn new(cert: &Certificate) -> Result<Rc<Self>, ChannelError> {
        Ok(Rc::new(Self {
            config: Arc::new(tls::client_config(cert)?),
            conns: RefCell::new(HashMap::new()),
        }))
    }

    /// Send a frame, connecting first if this peer has no open channel.
    pub async fn send(&self, addr: SocketAddr, frame: &Frame) -> Result<(), ChannelError> {
        let cached = self.conns.borrow_mut().remove(&addr);
        let mut conn = match cached {
            Some(conn) => conn,
            None => self.connect(addr).await?,
        };

        match write_frame(&mut conn, frame).await {
            Ok(()) => {
                self.conns.borrow_mut().insert(addr, conn);
                Ok(())
            }
            // a failed connection is dropped rather than retried; the
            // next transfer reconnects
            Err(e) => {
                log::debug!("clipboard: send to {addr} failed: {e}");
                Err(e)
            }
        }
    }

    async fn connect(&self, addr: SocketAddr) -> Result<ClientStream<TcpStream>, ChannelError> {
        log::debug!("clipboard: connecting to {addr}");
        let stream = TcpStream::connect(addr).await?;
        let name = ServerName::try_from(SERVER_NAME).map_err(|_| ChannelError::ServerName)?;
        let stream = TlsConnector::from(self.config.clone())
            .connect(name, stream)
            .await?;
        Ok(stream)
    }
}

/// Write one length-prefixed frame.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    frame: &Frame,
) -> Result<(), ChannelError> {
    let buf = frame::encode(frame);
    w.write_all(&buf).await?;
    w.flush().await?;
    Ok(())
}

/// Read one length-prefixed frame.
///
/// The declared length is checked against `max_len` before the body is
/// allocated, so a corrupted or hostile prefix cannot exhaust memory.
pub async fn read_frame<R: AsyncRead + Unpin>(
    r: &mut R,
    max_len: usize,
) -> Result<Frame, ChannelError> {
    let mut prefix = [0u8; 4];
    r.read_exact(&mut prefix).await?;
    let total = frame::peek_len(&prefix, max_len)?;
    let mut body = vec![0u8; total - 4];
    r.read_exact(&mut body).await?;
    Ok(frame::decode_payload(&body)?)
}

/// An empty clipboard frame, used by tests and as a keepalive shape.
pub fn frame_of(origin_seq: u64, name: &str, bytes: &[u8]) -> Frame {
    Frame {
        origin_seq,
        items: vec![Representation::new(name, bytes.to_vec())],
    }
}

/// Accepts peers in the background and hands finished frames to the
/// service loop, so a slow or stalled transfer never blocks input.
pub struct Listener {
    rx: tokio::sync::mpsc::Receiver<(SocketAddr, Frame)>,
}

impl Listener {
    pub async fn new(
        port: u16,
        cert: &Certificate,
        authorized: Arc<RwLock<HashMap<String, String>>>,
        max_len: usize,
    ) -> Result<Self, ChannelError> {
        let server = Server::bind(port, cert, authorized).await?;
        let (tx, rx) = tokio::sync::mpsc::channel(4);

        tokio::task::spawn_local(async move {
            loop {
                let (addr, mut stream) = match server.accept().await {
                    Ok(accepted) => accepted,
                    Err(e) => {
                        // a rejected peer is expected whenever someone
                        // without an authorized key probes the port
                        log::debug!("clipboard: accept failed: {e}");
                        continue;
                    }
                };
                let tx = tx.clone();
                tokio::task::spawn_local(async move {
                    loop {
                        match read_frame(&mut stream, max_len).await {
                            Ok(frame) => {
                                if tx.send((addr, frame)).await.is_err() {
                                    return;
                                }
                            }
                            Err(e) => {
                                log::debug!("clipboard: {addr} closed: {e}");
                                return;
                            }
                        }
                    }
                });
            }
        });

        Ok(Self { rx })
    }

    /// Next frame received from any peer.
    pub async fn recv(&mut self) -> Option<(SocketAddr, Frame)> {
        self.rx.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto;

    const LIMIT: usize = 1 << 20;

    fn cert() -> Certificate {
        Certificate::generate_self_signed(["ignored".to_owned()]).expect("cert")
    }

    fn authorized_for(cert: &Certificate) -> Arc<RwLock<HashMap<String, String>>> {
        let fingerprint = crypto::generate_fingerprint(cert.certificate[0].as_ref());
        Arc::new(RwLock::new(HashMap::from([(
            fingerprint,
            "test".to_owned(),
        )])))
    }

    #[tokio::test]
    async fn authorized_peer_transfers_and_reuses_the_connection() {
        let server_cert = cert();
        let client_cert = cert();
        let server = Server::bind(0, &server_cert, authorized_for(&client_cert))
            .await
            .expect("bind");
        let addr = server.local_addr().expect("addr");
        let client = Client::new(&client_cert).expect("client");

        let first = frame_of(1, "text/plain", b"one");
        let second = frame_of(2, "text/plain", b"two");

        let (sent, received) = tokio::join!(
            async {
                client.send(addr, &first).await?;
                client.send(addr, &second).await?;
                Ok::<(), ChannelError>(())
            },
            async {
                // one accept, two frames: the second send reused the
                // cached connection instead of dialing again
                let (_, mut stream) = server.accept().await?;
                let a = read_frame(&mut stream, LIMIT).await?;
                let b = read_frame(&mut stream, LIMIT).await?;
                Ok::<_, ChannelError>((a, b))
            }
        );

        sent.expect("sends succeed");
        let (a, b) = received.expect("receives succeed");
        assert_eq!(a, first);
        assert_eq!(b, second);
    }

    #[tokio::test]
    async fn unauthorized_peer_is_refused() {
        let server_cert = cert();
        let client_cert = cert();
        // empty authorized list: nobody may connect
        let server = Server::bind(0, &server_cert, Arc::default())
            .await
            .expect("bind");
        let addr = server.local_addr().expect("addr");
        let client = Client::new(&client_cert).expect("client");
        let frame = frame_of(1, "text/plain", b"secret");

        let (_sent, received) = tokio::join!(client.send(addr, &frame), async {
            let (_, mut stream) = server.accept().await?;
            read_frame(&mut stream, LIMIT).await
        });

        // the server must not hand out any clipboard bytes. Under TLS 1.3
        // the client's write can complete before the server has finished
        // checking its certificate, so the rejection shows up on the
        // accepting side, not as a send error.
        assert!(
            received.is_err(),
            "unauthorized peer must not be able to deliver a frame"
        );
    }

    #[tokio::test]
    async fn a_frame_over_the_limit_is_rejected_before_reading_the_body() {
        let server_cert = cert();
        let client_cert = cert();
        let server = Server::bind(0, &server_cert, authorized_for(&client_cert))
            .await
            .expect("bind");
        let addr = server.local_addr().expect("addr");
        let client = Client::new(&client_cert).expect("client");

        let big = frame_of(1, "text/plain", &vec![b'x'; 4096]);

        let (_, received) = tokio::join!(client.send(addr, &big), async {
            let (_, mut stream) = server.accept().await?;
            read_frame(&mut stream, 1024).await
        });

        match received {
            Err(ChannelError::Frame(DecodeError::TooLarge(len, limit))) => {
                assert_eq!(len, 4096 + 8 + 2 + 2 + 10 + 4);
                assert_eq!(limit, 1024);
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }
}
