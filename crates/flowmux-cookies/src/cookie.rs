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

impl Cookie {
    pub fn validate(&self) -> Result<(), String> {
        if self.host.is_empty()
            || self
                .host
                .chars()
                .any(|c| !c.is_ascii_alphanumeric() && c != '.' && c != '-')
            || self.name.is_empty()
            || self
                .name
                .bytes()
                .any(|c| c <= 32 || c >= 127 || b"()<>@,;:\\\"/[]?={}=".contains(&c))
            || self.value.chars().any(|c| c.is_control() || c == ';')
            || !self.path.starts_with('/')
            || self.path.chars().any(|c| c.is_control() || c == ';')
        {
            return Err("invalid imported cookie attributes".into());
        }
        if self.same_site == SameSite::None && !self.secure {
            return Err("SameSite=None cookie requires Secure".into());
        }
        if self.name.starts_with("__Secure-") && !self.secure
            || self.name.starts_with("__Host-")
                && (!self.secure || self.host.starts_with('.') || self.path != "/")
        {
            return Err("invalid cookie security prefix".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_cookie_attributes_cannot_change_scope_or_headers() {
        let valid = Cookie {
            host: "example.test".into(),
            name: "sid".into(),
            value: "fixture".into(),
            path: "/".into(),
            expires_at: None,
            secure: true,
            http_only: true,
            same_site: SameSite::Lax,
        };
        assert!(valid.validate().is_ok());
        for invalid in [
            Cookie {
                host: "example.test/path".into(),
                ..valid.clone()
            },
            Cookie {
                value: "x; Domain=other.test".into(),
                ..valid.clone()
            },
            Cookie {
                path: "/\r\n".into(),
                ..valid.clone()
            },
            Cookie {
                name: "__Host-sid".into(),
                host: ".example.test".into(),
                ..valid.clone()
            },
            Cookie {
                same_site: SameSite::None,
                secure: false,
                ..valid.clone()
            },
        ] {
            assert!(invalid.validate().is_err());
        }
    }
}
