//! Cross-platform clipboard access for lan-mouse.
//!
//! The clipboard is modelled as a [`Snapshot`]: the complete set of
//! representations (plain text, HTML, an image, …) that the source
//! application offered for one clipboard change. lan-mouse transfers the
//! whole set so the receiving application can pick whichever
//! representation it understands.

use std::fmt::Display;

pub use error::ClipboardError;

pub mod error;

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
}

impl Snapshot {
    /// total size of all representations in bytes
    pub fn size(&self) -> usize {
        self.items.iter().map(|i| i.bytes.len()).sum()
    }

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
        write!(f, "]")
    }
}

/// Read and write access to the system clipboard.
pub trait Clipboard: Send {
    /// Read every representation the clipboard currently offers.
    fn read(&self) -> Result<Snapshot, ClipboardError>;

    /// Replace the clipboard contents with the given representations.
    fn write(&self, snapshot: &Snapshot) -> Result<(), ClipboardError>;
}
