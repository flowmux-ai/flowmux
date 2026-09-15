// SPDX-License-Identifier: GPL-3.0-or-later
//! Parse OSC 9, 99, and 777 notification payloads and send desktop
//! notifications through `org.gtk.Notifications`. Launcher counts use
//! the separate Unity LauncherEntry D-Bus signal.

pub mod osc;
pub mod sender;
pub mod stream;

pub use osc::{parse_osc, OscNotification};
pub use sender::{DesktopNotifier, DESKTOP_FILE_BASENAME, OPEN_NOTIFICATION_ACTION};
pub use stream::OscExtractor;
