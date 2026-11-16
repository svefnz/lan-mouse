use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClipboardError {
    /// the platform clipboard could not be accessed
    #[error("clipboard access failed: `{0}`")]
    Access(String),
    /// the requested representation is not offered by the clipboard
    #[error("representation not available: `{0}`")]
    Unavailable(String),
}
