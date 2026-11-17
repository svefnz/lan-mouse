use crate::{
    capture::{Capture, CaptureType, ICaptureEvent},
    client::ClientManager,
    clipboard::{channel, frame::Frame, state as clipboard_state, state::SyncState},
    config::{Config, ConfigClient},
    connect::LanMouseConnection,
    crypto,
    dns::{DnsEvent, DnsResolver},
    emulation::{Emulation, EmulationEvent},
    listen::{LanMouseListener, ListenerCreationError},
};
use futures::StreamExt;
use input_clipboard::{InputClipboard, Snapshot, convert, map::Platform};
use lan_mouse_ipc::{
    AsyncFrontendListener, ClientHandle, FrontendEvent, FrontendRequest, IpcError,
    IpcListenerCreationError, Position, Status,
};
use log;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    io,
    net::{IpAddr, SocketAddr},
    rc::Rc,
    sync::{Arc, RwLock},
};
use thiserror::Error;
use tokio::{process::Command, signal, sync::Notify};

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error(transparent)]
    IpcListen(#[from] IpcListenerCreationError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    ListenError(#[from] ListenerCreationError),
    #[error("failed to load certificate: `{0}`")]
    Certificate(#[from] crypto::Error),
}

pub struct Service {
    /// configuration
    config: Config,
    /// certificate for the clipboard channel, kept so sharing can be
    /// started at runtime without reloading the key
    cert: webrtc_dtls::crypto::Certificate,
    /// input capture
    capture: Capture,
    /// input emulation
    emulation: Emulation,
    /// dns resolver
    resolver: DnsResolver,
    /// frontend listener
    frontend_listener: AsyncFrontendListener,
    /// authorized public key sha256 fingerprints
    authorized_keys: Arc<RwLock<HashMap<String, String>>>,
    /// (outgoing) client information
    client_manager: ClientManager,
    /// current port
    port: u16,
    /// the public key fingerprint for (D)TLS
    public_key_fingerprint: String,
    /// notify for pending frontend events
    frontend_event_pending: Notify,
    /// frontend events queued for sending
    pending_frontend_events: VecDeque<FrontendEvent>,
    /// status of input capture (enabled / disabled)
    capture_status: Status,
    /// status of input emulation (enabled / disabled)
    emulation_status: Status,
    /// keep track of registered connections to avoid duplicate barriers
    incoming_conns: HashSet<SocketAddr>,
    /// map from capture handle to connection info
    incoming_conn_info: HashMap<ClientHandle, Incoming>,
    next_trigger_handle: u64,
    /// local clipboard changes, `None` when clipboard sharing is off or
    /// the platform has no backend
    clipboard: Option<InputClipboard>,
    /// clipboard frames arriving from peers
    clipboard_listener: Option<channel::Listener>,
    /// sends clipboard frames to peers
    clipboard_client: Option<Rc<channel::Client>>,
    /// change counters and echo suppression
    clipboard_state: Rc<RefCell<SyncState>>,
    /// latest local snapshot, so entering a client does no clipboard call
    clipboard_snapshot: Option<Snapshot>,
    /// platform whose representation names are in use locally
    clipboard_platform: Platform,
    clipboard_max_bytes: usize,
    /// last clipboard status message shown to the frontend
    clipboard_status: Option<String>,
}

#[derive(Debug)]
struct Incoming {
    fingerprint: String,
    addr: SocketAddr,
    pos: Position,
}

/// Await the next local clipboard change, or never if the feature is off.
async fn next_clipboard_change(clipboard: Option<&mut InputClipboard>) -> Option<Snapshot> {
    match clipboard {
        Some(clipboard) => clipboard.next().await,
        None => std::future::pending().await,
    }
}

/// Await the next clipboard frame from a peer, or never if the feature is off.
async fn next_clipboard_frame(
    listener: Option<&mut channel::Listener>,
) -> Option<(SocketAddr, Frame)> {
    match listener {
        Some(listener) => listener.recv().await,
        None => std::future::pending().await,
    }
}

impl Service {
    pub async fn new(config: Config) -> Result<Self, ServiceError> {
        let client_manager = ClientManager::default();
        for client in config.clients() {
            client_manager.add_with_config(client);
        }

        // load certificate
        let cert = crypto::load_or_generate_key_and_cert(config.cert_path())?;
        let public_key_fingerprint = crypto::certificate_fingerprint(&cert);

        // create frontend communication adapter, exit if already running
        let frontend_listener = AsyncFrontendListener::new().await?;

        let authorized_keys = Arc::new(RwLock::new(config.authorized_fingerprints()));
        // listener + connection
        let listener =
            LanMouseListener::new(config.port(), cert.clone(), authorized_keys.clone()).await?;
        let conn = LanMouseConnection::new(cert.clone(), client_manager.clone());

        // input capture + emulation
        let capture_backend = config.capture_backend().map(|b| b.into());
        let capture = Capture::new(capture_backend, conn, config.release_bind());
        let emulation_backend = config.emulation_backend().map(|b| b.into());
        let emulation = Emulation::new(emulation_backend, listener);

        // clipboard sharing: its own TLS/TCP channel, independent of the
        // input path, so a large payload can never delay input
        let clipboard_max_bytes = config.clipboard_max_bytes();
        let (clipboard, clipboard_listener, clipboard_client) = if config.clipboard_enabled() {
            let max_len = clipboard_max_bytes + 64 * 1024;
            let clipboard = match InputClipboard::new(clipboard_max_bytes) {
                Ok(c) => Some(c),
                Err(e) => {
                    log::warn!("clipboard: backend unavailable: {e}");
                    None
                }
            };
            let listener = match channel::Listener::new(
                config.port(),
                &cert,
                authorized_keys.clone(),
                max_len,
            )
            .await
            {
                Ok(l) => Some(l),
                Err(e) => {
                    log::warn!("clipboard: channel listener unavailable: {e}");
                    None
                }
            };
            let client = match channel::Client::new(&cert) {
                Ok(c) => Some(c),
                Err(e) => {
                    log::warn!("clipboard: channel client unavailable: {e}");
                    None
                }
            };
            (clipboard, listener, client)
        } else {
            log::info!("clipboard sharing is disabled");
            (None, None, None)
        };

        // create dns resolver
        let resolver = DnsResolver::new()?;

        let port = config.port();
        let service = Self {
            config,
            cert,
            capture,
            emulation,
            frontend_listener,
            resolver,
            authorized_keys,
            public_key_fingerprint,
            client_manager,
            frontend_event_pending: Default::default(),
            port,
            pending_frontend_events: Default::default(),
            capture_status: Default::default(),
            emulation_status: Default::default(),
            incoming_conn_info: Default::default(),
            incoming_conns: Default::default(),
            next_trigger_handle: 0,
            clipboard,
            clipboard_listener,
            clipboard_client,
            clipboard_state: Rc::new(RefCell::new(SyncState::default())),
            clipboard_snapshot: None,
            clipboard_platform: input_clipboard::map::current_platform(),
            clipboard_max_bytes,
            clipboard_status: None,
        };
        Ok(service)
    }

    pub async fn run(&mut self) -> Result<(), ServiceError> {
        let active = self.client_manager.active_clients();
        for handle in active.iter() {
            // small hack: `activate_client()` checks, if the client
            // is already active in client_manager and does not create a
            // capture barrier in that case so we have to deactivate it first
            self.client_manager.deactivate_client(*handle);
        }

        for handle in active {
            self.activate_client(handle);
        }

        loop {
            tokio::select! {
                request = self.frontend_listener.next() => self.handle_frontend_request(request).await,
                _ = self.frontend_event_pending.notified() => self.handle_frontend_pending().await,
                event = self.emulation.event() => self.handle_emulation_event(event),
                event = self.capture.event() => self.handle_capture_event(event),
                event = self.resolver.event() => self.handle_resolver_event(event),
                _ = self.config.changed() => self.handle_config_change(),
                snapshot = next_clipboard_change(self.clipboard.as_mut()) => self.handle_clipboard_change(snapshot),
                frame = next_clipboard_frame(self.clipboard_listener.as_mut()) => self.handle_clipboard_frame(frame),
                r = signal::ctrl_c() => break r.expect("failed to wait for CTRL+C"),
            }
        }

        log::info!("terminating service ...");
        log::debug!("terminating capture ...");
        self.capture.terminate().await;
        log::debug!("terminating emulation ...");
        self.emulation.terminate().await;
        log::debug!("terminating dns resolver ...");
        self.resolver.terminate().await;
        log::debug!("terminating clipboard ...");
        if let Some(mut clipboard) = self.clipboard.take() {
            clipboard.terminate();
        }

        Ok(())
    }

    async fn handle_frontend_request(
        &mut self,
        request: Option<Result<FrontendRequest, IpcError>>,
    ) {
        let request = match request.expect("frontend listener closed") {
            Ok(r) => r,
            Err(e) => return log::error!("error receiving request: {e}"),
        };
        match request {
            // the handshake is consumed by the frontend listener; a Hello
            // from an established connection is a protocol violation —
            // answer with a fresh state sync
            FrontendRequest::Hello {
                version,
                capabilities,
            } => {
                log::warn!(
                    "unexpected handshake from an established frontend \
                    (version {version}, capabilities {capabilities:?})"
                );
                self.sync_frontend();
            }
            FrontendRequest::Activate(handle, active) => {
                self.set_client_active(handle, active);
                self.save_config();
            }
            FrontendRequest::AuthorizeKey(desc, fp) => {
                self.add_authorized_key(desc, fp);
                self.save_config();
            }
            FrontendRequest::ChangePort(port) => self.change_port(port),
            FrontendRequest::Create => {
                self.add_client();
                self.save_config();
            }
            FrontendRequest::Delete(handle) => {
                self.remove_client(handle);
                self.save_config();
            }
            FrontendRequest::EnableCapture => self.capture.reenable(),
            FrontendRequest::EnableEmulation => self.emulation.reenable(),
            FrontendRequest::Enumerate() => self.enumerate(),
            FrontendRequest::UpdateFixIps(handle, fix_ips) => {
                self.update_fix_ips(handle, fix_ips);
                self.save_config();
            }
            FrontendRequest::UpdateHostname(handle, host) => {
                self.update_hostname(handle, host);
                self.save_config();
            }
            FrontendRequest::UpdatePort(handle, port) => {
                self.update_port(handle, port);
                self.save_config();
            }
            FrontendRequest::UpdatePosition(handle, pos) => {
                self.update_pos(handle, pos);
                self.save_config();
            }
            FrontendRequest::ResolveDns(handle) => self.resolve(handle),
            FrontendRequest::Sync => self.sync_frontend(),
            FrontendRequest::RemoveAuthorizedKey(key) => {
                self.remove_authorized_key(key);
                self.save_config();
            }
            FrontendRequest::UpdateEnterHook(handle, enter_hook) => {
                self.update_enter_hook(handle, enter_hook)
            }
            FrontendRequest::UpdateLeaveHook(handle, leave_hook) => {
                self.update_leave_hook(handle, leave_hook)
            }
            FrontendRequest::SaveConfiguration => self.save_config(),
            FrontendRequest::SetClipboardEnabled(enabled) => {
                self.set_clipboard_enabled(enabled).await
            }
        }
    }

    fn save_config(&mut self) {
        let clients = self.client_manager.clients();
        let clients = clients
            .into_iter()
            .map(|(c, s)| ConfigClient {
                ips: HashSet::from_iter(c.fix_ips),
                hostname: c.hostname,
                port: c.port,
                pos: c.pos,
                active: s.active,
                enter_hook: c.cmd,
                leave_hook: c.leave_cmd,
            })
            .collect();
        self.config.set_clients(clients);
        let authorized_keys = self.authorized_keys.read().expect("lock").clone();
        self.config.set_authorized_keys(authorized_keys);
        self.config.set_port(self.port);
        if let Err(e) = self.config.write_back() {
            log::warn!("failed to write config: {e}");
        }
    }

    fn handle_config_change(&mut self) {
        for h in self.client_manager.registered_clients() {
            self.remove_client(h);
        }
        for c in self.config.clients() {
            let handle = self.client_manager.add_with_config(c);
            log::info!("added client {handle}");
            let (c, s) = self.client_manager.get_state(handle).unwrap();
            if s.active {
                self.client_manager.deactivate_client(handle);
                self.activate_client(handle);
            }
            self.notify_frontend(FrontendEvent::Created(handle, c, s));
        }
        let release_bind = self.config.release_bind();
        self.capture.set_release_bind(release_bind);
        let authorized_keys = self.config.authorized_fingerprints();
        self.authorized_keys
            .write()
            .unwrap()
            .clone_from(&authorized_keys);
        self.sync_frontend();
    }

    async fn handle_frontend_pending(&mut self) {
        while let Some(event) = self.pending_frontend_events.pop_front() {
            self.frontend_listener.broadcast(event).await;
        }
    }

    fn handle_emulation_event(&mut self, event: EmulationEvent) {
        match event {
            EmulationEvent::ConnectionAttempt { fingerprint } => {
                self.notify_frontend(FrontendEvent::ConnectionAttempt { fingerprint });
            }
            EmulationEvent::Entered {
                addr,
                pos,
                fingerprint,
            } => {
                // check if already registered
                if !self.incoming_conns.contains(&addr) {
                    self.add_incoming(addr, pos, fingerprint.clone());
                    self.notify_frontend(FrontendEvent::DeviceEntered {
                        fingerprint,
                        addr,
                        pos,
                    });
                } else {
                    self.update_incoming(addr, pos, fingerprint);
                }
            }
            EmulationEvent::Disconnected { addr } => {
                if let Some(addr) = self.remove_incoming(addr) {
                    self.notify_frontend(FrontendEvent::IncomingDisconnected(addr));
                }
            }
            EmulationEvent::PortChanged(port) => match port {
                Ok(port) => {
                    self.port = port;
                    self.notify_frontend(FrontendEvent::PortChanged(port, None));
                }
                Err(e) => self
                    .notify_frontend(FrontendEvent::PortChanged(self.port, Some(format!("{e}")))),
            },
            EmulationEvent::EmulationDisabled => {
                self.emulation_status = Status::Disabled;
                self.notify_frontend(FrontendEvent::EmulationStatus(self.emulation_status));
            }
            EmulationEvent::EmulationEnabled => {
                self.emulation_status = Status::Enabled;
                self.notify_frontend(FrontendEvent::EmulationStatus(self.emulation_status));
            }
            EmulationEvent::ReleaseNotify => self.capture.release(),
            EmulationEvent::Connected { addr, fingerprint } => {
                self.notify_frontend(FrontendEvent::DeviceConnected { addr, fingerprint });
            }
            EmulationEvent::PeerHello { addr, commit } => {
                // Map the peer's source addr back to its client handle
                // and stamp the commit. Skip if we don't have an
                // outgoing client configured for this peer (incoming-
                // only setup) — there's nowhere to display the version
                // in that case anyway.
                if let Some(handle) = self.client_manager.get_client(addr) {
                    self.client_manager.set_peer_commit(handle, Some(commit));
                    self.broadcast_client(handle);
                }
            }
        }
    }

    fn handle_capture_event(&mut self, event: ICaptureEvent) {
        match event {
            ICaptureEvent::CaptureBegin(handle) => {
                // we entered the capture zone for an incoming connection
                // => notify it that its capture should be released
                if let Some(incoming) = self.incoming_conn_info.get(&handle) {
                    self.emulation.send_leave_event(incoming.addr);
                }
            }
            ICaptureEvent::CaptureDisabled => {
                self.capture_status = Status::Disabled;
                self.notify_frontend(FrontendEvent::CaptureStatus(self.capture_status));
            }
            ICaptureEvent::CaptureEnabled => {
                self.capture_status = Status::Enabled;
                self.notify_frontend(FrontendEvent::CaptureStatus(self.capture_status));
            }
            ICaptureEvent::ClientEntered(handle) => {
                log::info!("entering client {handle} ...");
                self.spawn_hook_command(handle, HookKind::Enter);
                self.push_clipboard(handle);
            }
            ICaptureEvent::ClientLeft(handle) => {
                log::info!("leaving client {handle} ...");
                self.spawn_hook_command(handle, HookKind::Leave);
            }
        }
    }

    fn handle_clipboard_change(&mut self, snapshot: Option<Snapshot>) {
        let Some(snapshot) = snapshot else {
            return;
        };

        let hash = clipboard_state::hash(&snapshot);
        {
            let mut state = self.clipboard_state.borrow_mut();
            if state.take_echo(hash) {
                log::trace!("clipboard: ignoring the echo of our own write");
                return;
            }
            state.on_local_change();
        }

        log::debug!("clipboard: local change {snapshot}");

        // surface a degradation, otherwise clear a stale message
        let status = if snapshot.dropped.is_empty() {
            None
        } else {
            let dropped = snapshot
                .dropped
                .iter()
                .map(|(name, size)| format!("{name} ({size} bytes)"))
                .collect::<Vec<_>>()
                .join(", ");
            Some(format!(
                "clipboard: not synced, over the size limit: {dropped}"
            ))
        };
        self.clipboard_status = status.clone();
        self.notify_frontend(FrontendEvent::ClipboardStatus(status));

        self.clipboard_snapshot = Some(snapshot);
    }

    fn handle_clipboard_frame(&mut self, received: Option<(SocketAddr, Frame)>) {
        let Some((addr, frame)) = received else {
            return;
        };

        if self.clipboard_state.borrow().is_own_echo(frame.origin_seq) {
            log::trace!("clipboard: {addr} echoed our own content, ignoring");
            return;
        }

        let wire = Snapshot {
            items: frame.items,
            dropped: Vec::new(),
        };
        let local = convert::from_wire(self.clipboard_platform, &wire);
        log::debug!("clipboard: received {local} from {addr}");

        // record what we are about to write before writing it, so the
        // resulting change event is recognized as our own echo
        self.clipboard_state
            .borrow_mut()
            .expect_echo(clipboard_state::hash(&local));

        let Some(clipboard) = &self.clipboard else {
            return;
        };
        if let Err(e) = clipboard.write(&local) {
            log::warn!("clipboard: could not apply content from {addr}: {e}");
        }
    }

    /// Push the cached local snapshot to a client we just entered.
    fn push_clipboard(&mut self, handle: ClientHandle) {
        let Some(snapshot) = self.clipboard_snapshot.clone() else {
            return;
        };
        let Some(client) = self.clipboard_client.clone() else {
            return;
        };
        let Some(addr) = self.client_manager.active_addr(handle) else {
            return;
        };

        let seq = {
            let state = self.clipboard_state.borrow();
            if !state.should_push(handle) {
                log::trace!("clipboard: nothing new for {handle}");
                return;
            }
            state.local_seq()
        };

        let wire = convert::to_wire(self.clipboard_platform, &snapshot);
        if wire.size() > self.clipboard_max_bytes {
            log::debug!(
                "clipboard: {} bytes exceeds the configured limit, not pushing",
                wire.size()
            );
            return;
        }

        let frame = Frame {
            origin_seq: seq,
            items: wire.items,
        };
        let state = self.clipboard_state.clone();
        log::debug!(
            "clipboard: pushing {} representation(s) to {handle}",
            frame.items.len()
        );

        // off the service loop: a large transfer must not delay input
        tokio::task::spawn_local(async move {
            match client.send(addr, &frame).await {
                Ok(()) => state.borrow_mut().on_pushed(handle, seq),
                Err(e) => log::debug!("clipboard: push to {handle} failed: {e}"),
            }
        });
    }

    fn handle_resolver_event(&mut self, event: DnsEvent) {
        let handle = match event {
            DnsEvent::Resolving(handle) => {
                self.client_manager.set_resolving(handle, true);
                handle
            }
            DnsEvent::Resolved(handle, hostname, ips) => {
                self.client_manager.set_resolving(handle, false);
                if let Err(e) = &ips {
                    log::warn!("could not resolve {hostname}: {e}");
                }
                let ips = ips.unwrap_or_default();
                self.client_manager.set_dns_ips(handle, ips);
                handle
            }
        };
        self.broadcast_client(handle);
    }

    fn resolve(&self, handle: ClientHandle) {
        if let Some(hostname) = self.client_manager.get_hostname(handle) {
            self.resolver.resolve(handle, hostname);
        }
    }

    fn sync_frontend(&mut self) {
        self.enumerate();
        self.notify_frontend(FrontendEvent::EmulationStatus(self.emulation_status));
        self.notify_frontend(FrontendEvent::CaptureStatus(self.capture_status));
        self.notify_frontend(FrontendEvent::PortChanged(self.port, None));
        self.notify_frontend(FrontendEvent::PublicKeyFingerprint(
            self.public_key_fingerprint.clone(),
        ));
        let keys = self.authorized_keys.read().expect("lock").clone();
        self.notify_frontend(FrontendEvent::AuthorizedUpdated(keys));
        self.notify_frontend(FrontendEvent::ClipboardStatus(
            self.clipboard_status.clone(),
        ));
        self.notify_frontend(FrontendEvent::ClipboardSyncEnabled(
            self.config.clipboard_enabled(),
        ));
    }

    const ENTER_HANDLE_BEGIN: u64 = u64::MAX / 2 + 1;

    fn add_incoming(&mut self, addr: SocketAddr, pos: Position, fingerprint: String) {
        let handle = Self::ENTER_HANDLE_BEGIN + self.next_trigger_handle;
        self.next_trigger_handle += 1;
        self.capture.create(handle, pos, CaptureType::EnterOnly);
        self.incoming_conns.insert(addr);
        self.incoming_conn_info.insert(
            handle,
            Incoming {
                fingerprint,
                addr,
                pos,
            },
        );
    }

    fn update_incoming(&mut self, addr: SocketAddr, pos: Position, fingerprint: String) {
        let incoming = self
            .incoming_conn_info
            .iter_mut()
            .find(|(_, i)| i.addr == addr)
            .map(|(_, i)| i)
            .expect("no such client");
        let mut changed = false;
        if incoming.fingerprint != fingerprint {
            incoming.fingerprint = fingerprint.clone();
            changed = true;
        }
        if incoming.pos != pos {
            incoming.pos = pos;
            changed = true;
        }
        if changed {
            self.remove_incoming(addr);
            self.add_incoming(addr, pos, fingerprint.clone());
            self.notify_frontend(FrontendEvent::IncomingDisconnected(addr));
            self.notify_frontend(FrontendEvent::DeviceEntered {
                fingerprint,
                addr,
                pos,
            });
        }
    }

    fn remove_incoming(&mut self, addr: SocketAddr) -> Option<SocketAddr> {
        let handle = self
            .incoming_conn_info
            .iter()
            .find(|(_, incoming)| incoming.addr == addr)
            .map(|(k, _)| *k)?;
        self.capture.destroy(handle);
        self.incoming_conns.remove(&addr);
        self.incoming_conn_info
            .remove(&handle)
            .map(|incoming| incoming.addr)
    }

    fn notify_frontend(&mut self, event: FrontendEvent) {
        self.pending_frontend_events.push_back(event);
        self.frontend_event_pending.notify_one();
    }

    fn add_authorized_key(&mut self, desc: String, fp: String) {
        self.authorized_keys.write().expect("lock").insert(fp, desc);
        let keys = self.authorized_keys.read().expect("lock").clone();
        self.notify_frontend(FrontendEvent::AuthorizedUpdated(keys));
        self.notify_frontend(FrontendEvent::ClipboardStatus(
            self.clipboard_status.clone(),
        ));
    }

    fn remove_authorized_key(&mut self, fp: String) {
        self.authorized_keys.write().expect("lock").remove(&fp);
        let keys = self.authorized_keys.read().expect("lock").clone();
        self.notify_frontend(FrontendEvent::AuthorizedUpdated(keys));
        self.notify_frontend(FrontendEvent::ClipboardStatus(
            self.clipboard_status.clone(),
        ));
    }

    fn enumerate(&mut self) {
        let clients = self.client_manager.get_client_states();
        self.notify_frontend(FrontendEvent::Enumerate(clients));
    }

    fn add_client(&mut self) {
        let handle = self.client_manager.add_client();
        log::info!("added client {handle}");
        let (c, s) = self.client_manager.get_state(handle).unwrap();
        self.notify_frontend(FrontendEvent::Created(handle, c, s));
    }

    fn set_client_active(&mut self, handle: ClientHandle, active: bool) {
        if active {
            self.activate_client(handle);
        } else {
            self.deactivate_client(handle);
        }
    }

    fn deactivate_client(&mut self, handle: ClientHandle) {
        log::debug!("deactivating client {handle}");
        if self.client_manager.deactivate_client(handle) {
            self.capture.destroy(handle);
            self.broadcast_client(handle);
            log::info!("deactivated client {handle}");
        }
    }

    fn activate_client(&mut self, handle: ClientHandle) {
        log::debug!("activating client {handle}");

        /* resolve dns on activate */
        self.resolve(handle);

        /* deactivate potential other client at this position */
        let Some(pos) = self.client_manager.get_pos(handle) else {
            return;
        };

        if let Some(other) = self.client_manager.client_at(pos) {
            if other != handle {
                self.deactivate_client(other);
            }
        }

        /* activate the client */
        if self.client_manager.activate_client(handle) {
            /* notify capture and frontends */
            self.capture.create(handle, pos, CaptureType::Default);
            self.broadcast_client(handle);
            log::info!("activated client {handle} ({pos})");
        }
    }

    fn change_port(&mut self, port: u16) {
        if self.port != port {
            self.emulation.request_port_change(port);
        } else {
            self.notify_frontend(FrontendEvent::PortChanged(self.port, None));
        }
    }

    /// Apply a clipboard on/off request: update the config, save it and
    /// start or stop the clipboard backends at runtime.
    async fn set_clipboard_enabled(&mut self, enabled: bool) {
        if self.config.clipboard_enabled() == enabled {
            return;
        }
        self.config.set_clipboard_enabled(enabled);
        self.save_config();
        if enabled {
            self.enable_clipboard().await;
        } else {
            self.disable_clipboard();
        }
        // a stale degradation message must not survive the toggle
        self.clipboard_status = None;
        self.notify_frontend(FrontendEvent::ClipboardStatus(None));
        self.notify_frontend(FrontendEvent::ClipboardSyncEnabled(enabled));
    }

    /// start the clipboard watcher, TLS listener and client
    async fn enable_clipboard(&mut self) {
        if self.clipboard.is_none() {
            match InputClipboard::new(self.clipboard_max_bytes) {
                Ok(c) => self.clipboard = Some(c),
                Err(e) => log::warn!("clipboard: backend unavailable: {e}"),
            }
        }
        let max_len = self.clipboard_max_bytes + 64 * 1024;
        if self.clipboard_listener.is_none() {
            match channel::Listener::new(
                self.port,
                &self.cert,
                self.authorized_keys.clone(),
                max_len,
            )
            .await
            {
                Ok(l) => self.clipboard_listener = Some(l),
                Err(e) => log::warn!("clipboard: channel listener unavailable: {e}"),
            }
        }
        if self.clipboard_client.is_none() {
            match channel::Client::new(&self.cert) {
                Ok(c) => self.clipboard_client = Some(c),
                Err(e) => log::warn!("clipboard: channel client unavailable: {e}"),
            }
        }
    }

    /// stop clipboard sharing: drop the watcher, listener and client so no
    /// content is transferred until it is enabled again
    fn disable_clipboard(&mut self) {
        log::info!("clipboard sharing is disabled");
        if let Some(mut clipboard) = self.clipboard.take() {
            clipboard.terminate();
        }
        self.clipboard_listener = None;
        self.clipboard_client = None;
        self.clipboard_snapshot = None;
    }

    fn remove_client(&mut self, handle: ClientHandle) {
        if self
            .client_manager
            .remove_client(handle)
            .map(|(_, s)| s.active)
            .unwrap_or(false)
        {
            self.capture.destroy(handle);
        }
        self.notify_frontend(FrontendEvent::Deleted(handle));
    }

    fn update_fix_ips(&mut self, handle: ClientHandle, fix_ips: Vec<IpAddr>) {
        self.client_manager.set_fix_ips(handle, fix_ips);
        self.broadcast_client(handle);
    }

    fn update_hostname(&mut self, handle: ClientHandle, hostname: Option<String>) {
        log::info!("hostname changed: {hostname:?}");
        if self.client_manager.set_hostname(handle, hostname.clone()) {
            self.resolve(handle);
        }
        self.broadcast_client(handle);
    }

    fn update_port(&mut self, handle: ClientHandle, port: u16) {
        self.client_manager.set_port(handle, port);
        self.broadcast_client(handle);
    }

    fn update_pos(&mut self, handle: ClientHandle, pos: Position) {
        // update state in event input emulator & input capture
        if self.client_manager.set_pos(handle, pos) {
            self.deactivate_client(handle);
            self.activate_client(handle);
        }
        self.broadcast_client(handle);
    }

    fn update_enter_hook(&mut self, handle: ClientHandle, enter_hook: Option<String>) {
        self.client_manager.set_enter_hook(handle, enter_hook);
        self.broadcast_client(handle);
    }

    fn update_leave_hook(&mut self, handle: ClientHandle, leave_hook: Option<String>) {
        self.client_manager.set_leave_hook(handle, leave_hook);
        self.broadcast_client(handle);
    }

    fn broadcast_client(&mut self, handle: ClientHandle) {
        let event = self
            .client_manager
            .get_state(handle)
            .map(|(c, s)| FrontendEvent::State(handle, c, s))
            .unwrap_or(FrontendEvent::NoSuchClient(handle));
        self.notify_frontend(event);
    }

    fn spawn_hook_command(&self, handle: ClientHandle, kind: HookKind) {
        let cmd = match kind {
            HookKind::Enter => self.client_manager.get_enter_cmd(handle),
            HookKind::Leave => self.client_manager.get_leave_cmd(handle),
        };
        let Some(cmd) = cmd else { return };
        tokio::task::spawn_local(async move {
            log::info!("spawning {kind} hook for client {handle}");
            let mut child = match Command::new("sh").arg("-c").arg(cmd.as_str()).spawn() {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("could not execute {kind} hook for client {handle}: {e}");
                    return;
                }
            };
            match child.wait().await {
                Ok(s) => {
                    if s.success() {
                        log::info!("{kind} hook for client {handle} ({cmd}) exited successfully");
                    } else {
                        log::warn!("{kind} hook for client {handle} ({cmd}) exited with {s}");
                    }
                }
                Err(e) => log::warn!("{kind} hook for client {handle} ({cmd}): {e}"),
            }
        });
    }
}

#[derive(Clone, Copy, Debug)]
enum HookKind {
    Enter,
    Leave,
}

impl std::fmt::Display for HookKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HookKind::Enter => f.write_str("enter"),
            HookKind::Leave => f.write_str("leave"),
        }
    }
}
