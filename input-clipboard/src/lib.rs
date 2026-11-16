//! Cross-platform clipboard access for lan-mouse.
//!
//! The clipboard is modelled as a [`Snapshot`]: the complete set of
//! representations (plain text, HTML, an image, …) that the source
//! application offered for one clipboard change. lan-mouse transfers the
//! whole set so the receiving application can pick whichever
//! representation it understands.
//!
//! # Platform support
//!
//! | | text | html | rtf | image | file list | change watch |
//! |---|---|---|---|---|---|---|
//! | macOS | yes | yes | yes | yes | paths | 500 ms poll |
//! | Windows | yes | yes | yes | yes | paths | event |
//! | X11 | yes | yes | yes | yes | paths | event |
//! | Wayland | yes | yes | yes | yes | paths | event |
//! | other | none — the dummy backend reports the feature as unavailable |
//!
//! A file list is transferred as `text/uri-list` paths only; file
//! contents are never copied. Writing a file list replaces the whole
//! clipboard, because the platform APIs that set files clear it first.

use std::{
    fmt::Display,
    pin::Pin,
    task::{Context, Poll},
};

use futures_core::Stream;
use tokio::sync::mpsc::Receiver;

pub use error::ClipboardError;

pub mod cf_html;
pub mod concealed;
pub mod convert;
pub mod error;
pub mod map;
pub mod uri_list;

#[cfg(feature = "backend")]
mod backend;

#[cfg(not(feature = "backend"))]
mod dummy;

/// One representation of a clipboard change, e.g. plain text or an image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Representation {
    /// platform type name, e.g. `public.utf8-plain-text` or `text/html`
    pub name: String,
    /// raw bytes of the representation, unmodified
    pub bytes: Vec<u8>,
}

impl Representation {
    pub fn new(name: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            bytes,
        }
    }
}

impl Display for Representation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({} bytes)", self.name, self.bytes.len())
    }
}

/// The complete set of representations the clipboard offered for one change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub items: Vec<Representation>,
    /// Representations left out because they did not fit the size cap,
    /// as `(name, size)`. Reported to the user as a degradation.
    pub dropped: Vec<(String, usize)>,
}

impl Snapshot {
    /// total size of all retained representations in bytes
    pub fn size(&self) -> usize {
        self.items.iter().map(|i| i.bytes.len()).sum()
    }

    /// whether no representation carries any bytes
    pub fn is_empty(&self) -> bool {
        self.items.iter().all(|i| i.bytes.is_empty())
    }
}

impl Display for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "snapshot [")?;
        for (i, item) in self.items.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{item}")?;
        }
        write!(f, "]")?;
        if !self.dropped.is_empty() {
            write!(f, " (dropped {:?})", self.dropped)?;
        }
        Ok(())
    }
}

/// Read and write access to the system clipboard.
pub trait Clipboard: Send {
    /// Read every representation the clipboard currently offers.
    fn read(&self) -> Result<Snapshot, ClipboardError>;

    /// Replace the clipboard contents with the given representations.
    fn write(&self, snapshot: &Snapshot) -> Result<(), ClipboardError>;
}

/// Apply the size cap and drop content that must not be shared.
///
/// Returns `None` when nothing should be propagated at all: concealed
/// content, an empty clipboard, or nothing left after the cap.
#[cfg_attr(not(feature = "backend"), allow(dead_code))]
fn prepare(mut snapshot: Snapshot, max_bytes: usize) -> Option<Snapshot> {
    if concealed::is_concealed(&snapshot.items) {
        log::debug!("clipboard: content is concealed, not sharing");
        return None;
    }
    if snapshot.is_empty() {
        return None;
    }

    // smallest first, so plain text survives a cap that a screenshot
    // alone would exhaust
    snapshot.items.sort_by_key(|i| i.bytes.len());

    let mut kept = Vec::with_capacity(snapshot.items.len());
    let mut dropped = Vec::new();
    let mut budget = max_bytes;
    for item in snapshot.items {
        let len = item.bytes.len();
        if len <= budget {
            budget -= len;
            kept.push(item);
        } else {
            dropped.push((item.name, len));
        }
    }

    if kept.is_empty() {
        log::debug!("clipboard: nothing fits the {max_bytes} byte cap");
        return None;
    }
    if !dropped.is_empty() {
        log::debug!("clipboard: dropped {dropped:?} over the {max_bytes} byte cap");
    }

    Some(Snapshot {
        items: kept,
        dropped,
    })
}

/// Stops the clipboard watcher when dropped.
trait WatcherStop: Send {
    fn stop(self: Box<Self>);
}

#[cfg(feature = "backend")]
impl WatcherStop for clipboard_rs::WatcherShutdown {
    fn stop(self: Box<Self>) {
        (*self).stop();
    }
}

/// The system clipboard, observed as a stream of snapshots.
///
/// Changes are read on the watcher thread, so a caller that only drains
/// the stream never performs a blocking clipboard call.
pub struct InputClipboard {
    backend: Box<dyn Clipboard>,
    rx: Receiver<Snapshot>,
    stop: Option<Box<dyn WatcherStop>>,
}

impl InputClipboard {
    /// Start watching the clipboard.
    ///
    /// `max_bytes` bounds the retained snapshot: representations that do
    /// not fit are reported in [`Snapshot::dropped`] instead of held.
    pub fn new(max_bytes: usize) -> Result<Self, ClipboardError> {
        let (tx, rx) = tokio::sync::mpsc::channel(4);

        #[cfg(feature = "backend")]
        {
            let backend: Box<dyn Clipboard> = Box::new(backend::PlatformClipboard::new()?);
            let stop = Some(spawn_watcher(max_bytes, tx)?);
            Ok(Self { backend, rx, stop })
        }

        #[cfg(not(feature = "backend"))]
        {
            let _ = (max_bytes, tx);
            Ok(Self {
                backend: Box::new(dummy::DummyClipboard::new()),
                rx,
                stop: None,
            })
        }
    }

    /// Read the clipboard without waiting for a change.
    pub fn read(&self) -> Result<Snapshot, ClipboardError> {
        self.backend.read()
    }

    /// Replace the clipboard contents.
    pub fn write(&self, snapshot: &Snapshot) -> Result<(), ClipboardError> {
        self.backend.write(snapshot)
    }

    /// Stop watching the clipboard.
    pub fn terminate(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop.stop();
        }
    }
}

impl Stream for InputClipboard {
    type Item = Snapshot;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Snapshot>> {
        self.rx.poll_recv(cx)
    }
}

impl Drop for InputClipboard {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(feature = "backend")]
fn spawn_watcher(
    max_bytes: usize,
    tx: tokio::sync::mpsc::Sender<Snapshot>,
) -> Result<Box<dyn WatcherStop>, ClipboardError> {
    use clipboard_rs::{ClipboardHandler, ClipboardWatcher, ClipboardWatcherContext};

    struct ChangeHandler {
        tx: tokio::sync::mpsc::Sender<Snapshot>,
        backend: backend::PlatformClipboard,
        max_bytes: usize,
    }

    impl ClipboardHandler for ChangeHandler {
        fn on_clipboard_change(&mut self) {
            // reading here, on the watcher thread, keeps the consumer's
            // task free of blocking clipboard calls
            let snapshot = match self.backend.read() {
                Ok(snapshot) => snapshot,
                Err(e) => return log::debug!("clipboard: read failed: {e}"),
            };
            if let Some(snapshot) = prepare(snapshot, self.max_bytes) {
                // a full queue means the consumer is behind; dropping the
                // oldest change is better than blocking the watcher
                if let Err(e) = self.tx.blocking_send(snapshot) {
                    log::debug!("clipboard: consumer gone: {e}");
                }
            }
        }
    }

    let mut watcher = ClipboardWatcherContext::new()
        .map_err(|e| ClipboardError::Access(format!("watcher: {e}")))?;
    let stop = watcher.get_shutdown_channel();
    watcher.add_handler(ChangeHandler {
        tx,
        backend: backend::PlatformClipboard::new()?,
        max_bytes,
    });

    std::thread::Builder::new()
        .name("clipboard-watch".to_owned())
        .spawn(move || watcher.start_watch())
        .map_err(|e| ClipboardError::Access(format!("watcher thread: {e}")))?;

    Ok(Box::new(stop))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(name: &str, len: usize) -> Representation {
        Representation::new(name, vec![b'x'; len])
    }

    #[test]
    fn empty_clipboard_is_not_propagated() {
        let snapshot = Snapshot {
            items: vec![Representation::new("public.utf8-plain-text", vec![])],
            dropped: vec![],
        };
        assert!(prepare(snapshot, 1024).is_none());
    }

    #[test]
    fn concealed_content_is_not_propagated() {
        let snapshot = Snapshot {
            items: vec![
                rep("public.utf8-plain-text", 5),
                Representation::new("org.nspasteboard.ConcealedType", vec![]),
            ],
            dropped: vec![],
        };
        assert!(prepare(snapshot, 1024).is_none());
    }

    #[test]
    fn content_within_the_cap_is_kept_whole() {
        let snapshot = Snapshot {
            items: vec![rep("public.utf8-plain-text", 10), rep("public.png", 90)],
            dropped: vec![],
        };
        let prepared = prepare(snapshot, 100).expect("fits");
        assert_eq!(prepared.items.len(), 2);
        assert!(prepared.dropped.is_empty());
    }

    #[test]
    fn over_cap_content_degrades_to_the_small_representations() {
        let snapshot = Snapshot {
            items: vec![rep("public.png", 5000), rep("public.utf8-plain-text", 10)],
            dropped: vec![],
        };
        let prepared = prepare(snapshot, 100).expect("text fits");
        assert_eq!(prepared.items.len(), 1);
        assert_eq!(prepared.items[0].name, "public.utf8-plain-text");
        assert_eq!(prepared.dropped, vec![("public.png".to_owned(), 5000)]);
    }

    #[test]
    fn nothing_fitting_the_cap_yields_no_snapshot() {
        let snapshot = Snapshot {
            items: vec![rep("public.png", 5000)],
            dropped: vec![],
        };
        assert!(prepare(snapshot, 100).is_none());
    }
}
