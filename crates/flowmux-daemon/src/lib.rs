// SPDX-License-Identifier: GPL-3.0-or-later
//! Reusable state store, headless IPC handler, and tmux compatibility bridge.
//! The GUI wraps the handler and supplies widget effects through its GTK
//! command bridge.

pub mod agent_host;
pub mod chat_runtime;
pub mod handler;
#[cfg(target_os = "linux")]
pub mod process_scope;
pub mod state_store;
pub mod tmux_compat;

pub use handler::DaemonHandler;
pub use state_store::{CloseOutcome, LocatedAgentPresence, StateStore};
