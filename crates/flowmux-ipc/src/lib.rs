// SPDX-License-Identifier: GPL-3.0-or-later
//! Newline-delimited JSON between the GUI and CLI over a Unix domain socket.
//! Each line is a complete [`Envelope`]. The GUI uses a per-PID socket; path
//! selection and the stable external-CLI fallback live in `flowmux-config`.

pub mod client;
pub mod protocol;
pub mod server;
pub mod tmux_compat;

pub use protocol::{Envelope, Request, Response, RpcError};

/// Companion endpoint for hooks and status reads. Append to the resolved socket
/// path so aliases cannot accidentally route requests to another instance.
pub fn control_socket_path(socket: &std::path::Path) -> std::path::PathBuf {
    let mut path = socket.as_os_str().to_os_string();
    path.push(".ctl");
    path.into()
}
