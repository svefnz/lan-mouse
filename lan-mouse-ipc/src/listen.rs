use futures::{Stream, StreamExt};
#[cfg(unix)]
use std::path::PathBuf;
use std::{
    io::ErrorKind,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio_stream::wrappers::LinesStream;

#[cfg(unix)]
use tokio::net::UnixListener;
#[cfg(unix)]
use tokio::net::UnixStream;

#[cfg(windows)]
use tokio::net::TcpListener;
#[cfg(windows)]
use tokio::net::TcpStream;

use crate::{
    FrontendEvent, FrontendRequest, IPC_PROTOCOL_VERSION, IpcError, IpcListenerCreationError,
};

/// request lines of one connected frontend
#[cfg(unix)]
type RequestLines = LinesStream<BufReader<ReadHalf<UnixStream>>>;
#[cfg(windows)]
type RequestLines = LinesStream<BufReader<ReadHalf<TcpStream>>>;

/// write half of one connected frontend
#[cfg(unix)]
type RequestTx = WriteHalf<UnixStream>;
#[cfg(windows)]
type RequestTx = WriteHalf<TcpStream>;

/// One connected frontend: its write half for broadcasts, its request stream
/// and the state of the protocol handshake. A connection whose first message
/// is not [`FrontendRequest::Hello`] is treated as a legacy frontend
/// (version 0, no capabilities).
struct FrontendConnection {
    tx: RequestTx,
    lines: RequestLines,
    handshake: Handshake,
}

enum Handshake {
    /// waiting for the frontend's first message to decide between a
    /// negotiated and a legacy frontend
    Pending,
    /// a handshake reply is being written; afterwards the connection either
    /// becomes established (`accept`) or is rejected and torn down
    Replying {
        buf: Vec<u8>,
        offset: usize,
        accept: bool,
    },
    /// handshake finished (or never needed); requests flow through
    Established,
}

impl Handshake {
    /// reply with `event`, then either establish or reject the connection
    fn replying(event: &FrontendEvent, accept: bool) -> Self {
        let mut json = serde_json::to_string(event).expect("handshake reply serializes");
        json.push('\n');
        Handshake::Replying {
            buf: json.into_bytes(),
            offset: 0,
            accept,
        }
    }
}

impl FrontendConnection {
    /// Poll the connection: drive an outstanding handshake and yield the next
    /// request. `None` ends the connection.
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<FrontendRequest, IpcError>>> {
        loop {
            if matches!(self.handshake, Handshake::Replying { .. }) {
                match self.poll_reply(cx) {
                    ReplyOutcome::InProgress => return Poll::Pending,
                    ReplyOutcome::Rejected => return Poll::Ready(None),
                    ReplyOutcome::Established => {} // fall through: poll the request stream so the
                                                    // connection's read interest is registered again
                }
            }
            if matches!(self.handshake, Handshake::Pending) {
                // the first message decides between negotiated and legacy
                let Some(line) = std::task::ready!(self.lines.poll_next_unpin(cx)) else {
                    // frontend disconnected before its first message
                    return Poll::Ready(None);
                };
                let request = line
                    .map_err(IpcError::from)
                    .and_then(|l| serde_json::from_str(l.as_str()).map_err(IpcError::from));
                match request {
                    Ok(FrontendRequest::Hello {
                        version,
                        capabilities,
                    }) => {
                        if version == IPC_PROTOCOL_VERSION {
                            log::debug!(
                                "frontend handshake: version {version}, capabilities {capabilities:?}"
                            );
                            self.handshake = Handshake::replying(
                                &FrontendEvent::Hello {
                                    version: IPC_PROTOCOL_VERSION,
                                    capabilities: vec![],
                                },
                                true,
                            );
                            // drive the reply in the next iteration
                            continue;
                        }
                        log::warn!(
                            "frontend handshake: unsupported protocol version {version}, rejecting"
                        );
                        self.handshake = Handshake::replying(
                            &FrontendEvent::Error(format!(
                                "incompatible frontend protocol version {version}: \
                                 this service speaks version {IPC_PROTOCOL_VERSION}"
                            )),
                            false,
                        );
                        continue;
                    }
                    // anything else is a legacy frontend; its first request
                    // still has to be delivered
                    request => {
                        log::debug!("legacy frontend connected (no handshake)");
                        self.handshake = Handshake::Established;
                        return Poll::Ready(Some(request));
                    }
                }
            }
            return self.poll_requests(cx);
        }
    }

    /// write the pending handshake reply, then establish or reject
    fn poll_reply(&mut self, cx: &mut Context<'_>) -> ReplyOutcome {
        let (buf, mut offset, accept) =
            match std::mem::replace(&mut self.handshake, Handshake::Established) {
                Handshake::Replying {
                    buf,
                    offset,
                    accept,
                } => (buf, offset, accept),
                _ => unreachable!("poll_reply called without a pending reply"),
            };
        loop {
            if offset == buf.len() {
                break;
            }
            match Pin::new(&mut self.tx).poll_write(cx, &buf[offset..]) {
                Poll::Pending => {
                    self.handshake = Handshake::Replying {
                        buf,
                        offset,
                        accept,
                    };
                    return ReplyOutcome::InProgress;
                }
                Poll::Ready(Ok(0)) | Poll::Ready(Err(_)) => {
                    log::debug!("handshake reply could not be written, dropping connection");
                    return ReplyOutcome::Rejected;
                }
                Poll::Ready(Ok(n)) => offset += n,
            }
        }
        if accept {
            ReplyOutcome::Established
        } else {
            // rejected: tear the connection down after the error reply
            ReplyOutcome::Rejected
        }
    }

    fn poll_requests(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<FrontendRequest, IpcError>>> {
        let line = std::task::ready!(self.lines.poll_next_unpin(cx));
        Poll::Ready(line.map(|l| {
            l.map_err(IpcError::from)
                .and_then(|l| serde_json::from_str(l.as_str()).map_err(IpcError::from))
        }))
    }
}

/// result of writing the pending handshake reply
enum ReplyOutcome {
    /// more of the reply left to write
    InProgress,
    /// reply written and the connection established
    Established,
    /// reply written and the connection rejected
    Rejected,
}

pub struct AsyncFrontendListener {
    #[cfg(windows)]
    listener: TcpListener,
    #[cfg(unix)]
    listener: UnixListener,
    #[cfg(unix)]
    socket_path: PathBuf,
    /// connected frontends
    connections: Vec<FrontendConnection>,
}

impl AsyncFrontendListener {
    pub async fn new() -> Result<Self, IpcListenerCreationError> {
        #[cfg(unix)]
        let (socket_path, listener) = {
            let socket_path = crate::default_socket_path()?;

            log::debug!("remove socket: {socket_path:?}");
            if socket_path.exists() {
                // try to connect to see if some other instance
                // of lan-mouse is already running
                match UnixStream::connect(&socket_path).await {
                    // connected -> lan-mouse is already running
                    Ok(_) => return Err(IpcListenerCreationError::AlreadyRunning),
                    // lan-mouse is not running but a socket was left behind
                    Err(e) => {
                        log::debug!("{socket_path:?}: {e} - removing left behind socket");
                        let _ = std::fs::remove_file(&socket_path);
                    }
                }
            }
            let listener = match UnixListener::bind(&socket_path) {
                Ok(ls) => ls,
                // some other lan-mouse instance has bound the socket in the meantime
                Err(e) if e.kind() == ErrorKind::AddrInUse => {
                    return Err(IpcListenerCreationError::AlreadyRunning);
                }
                Err(e) => return Err(IpcListenerCreationError::Bind(e)),
            };
            (socket_path, listener)
        };

        #[cfg(windows)]
        let listener = match TcpListener::bind("127.0.0.1:5252").await {
            Ok(ls) => ls,
            // some other lan-mouse instance has bound the socket in the meantime
            Err(e) if e.kind() == ErrorKind::AddrInUse => {
                return Err(IpcListenerCreationError::AlreadyRunning);
            }
            Err(e) => return Err(IpcListenerCreationError::Bind(e)),
        };

        let adapter = Self {
            listener,
            #[cfg(unix)]
            socket_path,
            connections: vec![],
        };

        Ok(adapter)
    }

    pub async fn broadcast(&mut self, notify: FrontendEvent) {
        // encode event
        let mut json = serde_json::to_string(&notify).unwrap();
        json.push('\n');

        // TODO do simultaneously
        let mut dead = vec![];
        for (i, connection) in self.connections.iter_mut().enumerate() {
            // write len + payload
            if connection.tx.write_all(json.as_bytes()).await.is_err() {
                dead.push(i);
            }
        }
        for i in dead.into_iter().rev() {
            self.connections.remove(i);
        }
    }
}

#[cfg(unix)]
impl Drop for AsyncFrontendListener {
    fn drop(&mut self) {
        log::debug!("remove socket: {:?}", self.socket_path);
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

impl Stream for AsyncFrontendListener {
    type Item = Result<FrontendRequest, IpcError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // requests from connected frontends
        let mut item = None;
        let mut dead = None;
        for (i, connection) in self.connections.iter_mut().enumerate() {
            match connection.poll(cx) {
                Poll::Ready(None) => dead = Some(i),
                Poll::Ready(Some(request)) => {
                    item = Some(request);
                    break;
                }
                Poll::Pending => {}
            }
        }
        if let Some(i) = dead {
            log::debug!("frontend disconnected");
            self.connections.remove(i);
        }
        if let Some(item) = item {
            return Poll::Ready(Some(item));
        }

        // newly accepted frontends
        let mut sync = false;
        while let Poll::Ready(Ok((stream, _))) = self.listener.poll_accept(cx) {
            let (rx, tx) = tokio::io::split(stream);
            let buf_reader = BufReader::new(rx);
            let lines = LinesStream::new(buf_reader.lines());
            self.connections.push(FrontendConnection {
                tx,
                lines,
                handshake: Handshake::Pending,
            });
            sync = true;
        }
        if sync {
            Poll::Ready(Some(Ok(FrontendRequest::Sync)))
        } else {
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AsyncFrontendEventReader, AsyncFrontendRequestWriter, connect_async};
    use futures::StreamExt;

    /// minimal stand-in for the service: answers state syncs and
    /// enumerations with an empty client list
    async fn run_fake_service(mut listener: AsyncFrontendListener) {
        while let Some(request) = listener.next().await {
            match request.expect("request") {
                FrontendRequest::Sync | FrontendRequest::Enumerate() => {
                    listener
                        .broadcast(FrontendEvent::Enumerate(Vec::new()))
                        .await;
                }
                _ => {}
            }
        }
    }

    /// point the socket path at a private directory so the tests do not
    /// collide with a running lan-mouse instance. Windows tests use the
    /// fixed TCP port and cannot be isolated this way.
    fn use_private_socket_dir() {
        // not std::env::temp_dir(): TMPDIR on macOS is longer than the
        // 104-byte sun_path limit of a unix socket address
        #[cfg(unix)]
        let dir = std::path::PathBuf::from("/tmp")
            .join(format!("lan-mouse-ipc-test-{}", std::process::id()));
        #[cfg(target_os = "macos")]
        let socket_parent = dir.join("Library").join("Caches");
        #[cfg(target_os = "macos")]
        std::fs::create_dir_all(&socket_parent).expect("create test socket dir");
        #[cfg(all(unix, not(target_os = "macos")))]
        std::fs::create_dir_all(&dir).expect("create test socket dir");
        #[cfg(all(unix, not(target_os = "macos")))]
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        #[cfg(target_os = "macos")]
        std::env::set_var("HOME", &dir);
    }

    /// fake service that is guaranteed to release the socket before
    /// the next scenario starts
    struct FakeService {
        shutdown: tokio::sync::oneshot::Sender<()>,
        handle: tokio::task::JoinHandle<()>,
    }

    impl FakeService {
        async fn start() -> Self {
            use_private_socket_dir();
            let listener = AsyncFrontendListener::new().await.expect("listener");
            let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
            let handle = tokio::spawn(async move {
                tokio::select! {
                    _ = run_fake_service(listener) => {}
                    _ = shutdown_rx => {}
                }
            });
            Self { shutdown, handle }
        }

        async fn stop(self) {
            let _ = self.shutdown.send(());
            self.handle.await.expect("service task");
        }
    }

    async fn connect_client() -> (AsyncFrontendEventReader, AsyncFrontendRequestWriter) {
        connect_async(None).await.expect("connect")
    }

    /// read events until `f` matches, tolerating unrelated events
    async fn expect_event(
        reader: &mut AsyncFrontendEventReader,
        what: &str,
        f: impl Fn(&FrontendEvent) -> bool,
    ) -> FrontendEvent {
        for _ in 0..16 {
            match reader.next().await {
                Some(Ok(event)) => {
                    if f(&event) {
                        return event;
                    }
                }
                Some(Err(e)) => panic!("unexpected error while waiting for {what}: {e}"),
                None => panic!("connection closed while waiting for {what}"),
            }
        }
        panic!("no {what} received");
    }

    /// a v1 frontend negotiates and keeps working afterwards
    async fn v1_client_negotiates() {
        eprintln!("trace:   start service");
        let service = FakeService::start().await;
        eprintln!("trace:   connect");
        let (mut reader, mut writer) = connect_client().await;
        eprintln!("trace:   connected, sending hello");

        writer
            .request(FrontendRequest::Hello {
                version: IPC_PROTOCOL_VERSION,
                capabilities: vec!["test-capability".to_string()],
            })
            .await
            .expect("hello");
        eprintln!("trace:   hello sent, reading reply");
        let event = expect_event(&mut reader, "hello reply", |e| {
            matches!(e, FrontendEvent::Hello { .. })
        })
        .await;
        let FrontendEvent::Hello {
            version,
            capabilities,
        } = event
        else {
            unreachable!()
        };
        assert_eq!(version, IPC_PROTOCOL_VERSION);
        assert!(capabilities.is_empty());
        eprintln!("trace:   hello reply received");

        writer
            .request(FrontendRequest::Enumerate())
            .await
            .expect("enumerate");
        expect_event(&mut reader, "enumerate reply", |e| {
            matches!(e, FrontendEvent::Enumerate(_))
        })
        .await;
        service.stop().await;
    }

    /// a legacy frontend (first message is not `Hello`) still receives state
    async fn legacy_client_receives_state() {
        let service = FakeService::start().await;
        let (mut reader, mut writer) = connect_client().await;

        writer
            .request(FrontendRequest::Enumerate())
            .await
            .expect("enumerate");
        expect_event(&mut reader, "enumerate reply", |e| {
            matches!(e, FrontendEvent::Enumerate(_))
        })
        .await;
        service.stop().await;
    }

    /// a version the service does not support is rejected with an explicit
    /// error and the connection is closed
    async fn unsupported_version_is_rejected() {
        let service = FakeService::start().await;
        let (mut reader, mut writer) = connect_client().await;

        writer
            .request(FrontendRequest::Hello {
                version: IPC_PROTOCOL_VERSION + 1,
                capabilities: vec![],
            })
            .await
            .expect("hello");
        let event = expect_event(&mut reader, "error reply", |e| {
            matches!(e, FrontendEvent::Error(_))
        })
        .await;
        let FrontendEvent::Error(message) = event else {
            unreachable!()
        };
        assert!(
            message.contains("version"),
            "error does not mention the version: {message}"
        );

        let closed = matches!(reader.next().await, None | Some(Err(_)));
        assert!(closed, "connection not closed after rejection");
        service.stop().await;
    }

    /// the scenarios share the socket path and must not run in parallel
    #[tokio::test]
    async fn frontend_handshake() {
        v1_client_negotiates().await;
        legacy_client_receives_state().await;
        unsupported_version_is_rejected().await;
    }
}
