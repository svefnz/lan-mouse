//! Clipboard transfer between peers.
//!
//! Clipboard content travels on its own TLS-over-TCP channel bound to the
//! same port as the input protocol, so a large payload can never delay
//! pointer or key events.

pub mod channel;
pub mod frame;
pub mod state;
pub mod tls;
