// SPDX-License-Identifier: GPL-3.0-or-later
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cookie {
    pub host: String,
    pub name: String,
    pub value: String,
    pub path: String,
    /// UTC expiration, serialized as RFC 3339; `None` denotes a session cookie.
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: SameSite,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SameSite {
    #[default]
    Lax,
    Strict,
    None,
    NoRestriction,
}
