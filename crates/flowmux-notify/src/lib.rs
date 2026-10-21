// SPDX-License-Identifier: GPL-3.0-or-later
//! Parse OSC 9, 99, and 777 notification payloads and send desktop
//! notifications through the platform's desktop service.

pub mod osc;
#[cfg(not(target_os = "macos"))]
pub mod sender;
#[cfg(target_os = "macos")]
#[path = "macos.rs"]
pub mod sender;
pub mod stream;

pub use osc::{parse_osc, OscNotification};
pub use sender::{DesktopNotifier, DESKTOP_FILE_BASENAME, OPEN_NOTIFICATION_ACTION};
pub use stream::OscExtractor;
