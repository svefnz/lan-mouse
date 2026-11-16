//! Fallback used when no platform clipboard backend is available.

use crate::{Clipboard, ClipboardError, Snapshot};

pub(crate) struct DummyClipboard;

impl DummyClipboard {
    pub(crate) fn new() -> Self {
        log::warn!("clipboard sharing unavailable on this platform");
        Self
    }
}

impl Clipboard for DummyClipboard {
    fn read(&self) -> Result<Snapshot, ClipboardError> {
        Ok(Snapshot::default())
    }

    fn write(&self, _: &Snapshot) -> Result<(), ClipboardError> {
        Ok(())
    }
}
