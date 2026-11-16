//! Platform clipboard access through `clipboard-rs`.
//!
//! Representations are read and written by their raw type name so no
//! information is lost in translation; `clipboard-rs` only supplies the
//! platform plumbing (pasteboard, X11/INCR, `CF_HDROP`, …).

use clipboard_rs::{Clipboard as _, ClipboardContent, ClipboardContext};

use crate::{Clipboard, ClipboardError, Representation, Snapshot, map, uri_list};

/// Names that carry a file list. They are read through `get_files` so the
/// platform's own encoding (`CF_HDROP` is a binary structure, not a URI
/// list) is decoded for us.
const FILE_LIST_NAMES: &[&str] = &[
    "public.file-url",
    "NSFilenamesPboardType",
    "CF_HDROP",
    "text/uri-list",
];

pub(crate) struct PlatformClipboard {
    ctx: ClipboardContext,
}

impl PlatformClipboard {
    pub(crate) fn new() -> Result<Self, ClipboardError> {
        ClipboardContext::new()
            .map(|ctx| Self { ctx })
            .map_err(|e| ClipboardError::Access(e.to_string()))
    }
}

impl Clipboard for PlatformClipboard {
    fn read(&self) -> Result<Snapshot, ClipboardError> {
        let names = self
            .ctx
            .available_formats()
            .map_err(|e| ClipboardError::Access(e.to_string()))?;

        let mut items = Vec::with_capacity(names.len() + 1);
        for name in names {
            if FILE_LIST_NAMES.contains(&name.as_str()) {
                continue;
            }
            match self.ctx.get_buffer(&name) {
                Ok(bytes) => items.push(Representation::new(name, bytes)),
                // a representation can disappear between listing and
                // reading when the source application is slow; that is
                // not a reason to fail the whole snapshot
                Err(e) => log::debug!("clipboard: skipping `{name}`: {e}"),
            }
        }

        match self.ctx.get_files() {
            Ok(paths) if !paths.is_empty() => items.push(Representation::new(
                "text/uri-list",
                uri_list::from_paths(&paths).into_bytes(),
            )),
            Ok(_) => {}
            Err(e) => log::debug!("clipboard: no file list: {e}"),
        }

        Ok(Snapshot {
            items,
            dropped: Vec::new(),
        })
    }

    fn write(&self, snapshot: &Snapshot) -> Result<(), ClipboardError> {
        // A file list is written on its own: setting files clears the
        // clipboard first, so mixing it with other representations would
        // drop one side. Copying a file is unambiguous enough that this
        // is the behaviour users expect.
        if let Some(files) = snapshot
            .items
            .iter()
            .find(|i| map::to_canonical(&i.name) == Some("text/uri-list"))
        {
            let paths = uri_list::to_paths(&String::from_utf8_lossy(&files.bytes));
            if paths.is_empty() {
                return Err(ClipboardError::Unavailable("text/uri-list".to_owned()));
            }
            if snapshot.items.len() > 1 {
                log::debug!(
                    "clipboard: writing file list only, dropping {} other representations",
                    snapshot.items.len() - 1
                );
            }
            return self
                .ctx
                .set_files(paths)
                .map_err(|e| ClipboardError::Access(e.to_string()));
        }

        if snapshot.items.is_empty() {
            return Ok(());
        }

        // every representation becomes a type of the same pasteboard
        // item, which is what a native copy produces
        let contents = snapshot
            .items
            .iter()
            .map(|i| ClipboardContent::Other(i.name.clone(), i.bytes.clone()))
            .collect::<Vec<_>>();

        self.ctx
            .set(contents)
            .map_err(|e| ClipboardError::Access(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trips through the real system clipboard, so it needs a
    /// desktop session. Run with `cargo test -p input-clipboard -- --ignored`.
    #[test]
    #[ignore = "needs a real system clipboard"]
    fn write_then_read_returns_the_same_representations() {
        let clipboard = PlatformClipboard::new().expect("clipboard");
        let sent = Snapshot {
            items: vec![
                Representation::new("text/plain", b"lan-mouse clipboard test".to_vec()),
                Representation::new("text/html", b"<b>lan-mouse</b>".to_vec()),
            ],
            dropped: Vec::new(),
        };
        clipboard.write(&sent).expect("write");

        let read = clipboard.read().expect("read");
        let names: Vec<&str> = read.items.iter().map(|i| i.name.as_str()).collect();
        assert!(
            names.contains(&"text/plain") || names.contains(&"public.utf8-plain-text"),
            "plain text missing from {names:?}"
        );

        let text = read
            .items
            .iter()
            .find(|i| map::to_canonical(&i.name) == Some("text/plain"))
            .expect("plain text");
        assert_eq!(text.bytes, b"lan-mouse clipboard test");
    }
}
