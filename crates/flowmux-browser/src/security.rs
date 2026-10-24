// SPDX-License-Identifier: GPL-3.0-or-later

/// Display only scheme/authority; credentials, path, query and fragment are not permission labels.
pub fn display_origin(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return "Unknown origin".into();
    };
    if !matches!(scheme, "http" | "https") {
        return "Non-web origin".into();
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or_default();
    if host.is_empty() || host.chars().any(char::is_control) {
        return "Unknown origin".into();
    }
    format!("{scheme}://{host}")
}

/// WKSecurityOrigin uses zero for an unspecified/default port.
pub fn display_security_origin(scheme: &str, host: &str, port: i64) -> String {
    if host.is_empty() || !(0..=65535).contains(&port) {
        return "Unknown origin".into();
    }
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.into()
    };
    let authority = if port == 0 {
        host
    } else {
        format!("{host}:{port}")
    };
    display_origin(&format!("{scheme}://{authority}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn origin_hides_credentials_and_paths() {
        assert_eq!(
            super::display_security_origin("https", "example.com", 0),
            "https://example.com"
        );
        assert_eq!(
            super::display_security_origin("http", "::1", 8080),
            "http://[::1]:8080"
        );
        assert_eq!(
            super::display_security_origin("https", "", 0),
            "Unknown origin"
        );
        assert_eq!(
            super::display_origin("https://user:secret@example.com:8443/private?token=x"),
            "https://example.com:8443"
        );
        assert_eq!(
            super::display_origin("file:///private/secret"),
            "Non-web origin"
        );
    }
}
