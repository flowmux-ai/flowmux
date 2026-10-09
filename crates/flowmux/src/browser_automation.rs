// SPDX-License-Identifier: GPL-3.0-or-later
//! Validate opt-in automation before WebKit starts its inspector listener.

pub(crate) fn requested() -> anyhow::Result<bool> {
    let enabled = std::env::var("FLOWMUX_WEBKIT_AUTOMATION").ok();
    let address = std::env::var("WEBKIT_INSPECTOR_SERVER").ok();
    let requested = validate(enabled.as_deref(), address.as_deref())?;
    if requested {
        anyhow::ensure!(
            cfg!(target_os = "linux"),
            "WebKitWebDriver requires Linux WebKitGTK; WKWebView does not expose this API"
        );
        #[cfg(target_os = "linux")]
        anyhow::ensure!(
            (
                webkit6::functions::major_version(),
                webkit6::functions::minor_version()
            ) >= (2, 46),
            "Flowmux WebDriver sessions require WebKitGTK 2.46 or later"
        );
    }
    Ok(requested)
}

fn validate(enabled: Option<&str>, address: Option<&str>) -> anyhow::Result<bool> {
    match enabled {
        None | Some("0") => return Ok(false),
        Some("1") => {}
        _ => anyhow::bail!("FLOWMUX_WEBKIT_AUTOMATION must be 0 or 1"),
    }
    let address: std::net::SocketAddr = address
        .ok_or_else(|| {
            anyhow::anyhow!("Set WEBKIT_INSPECTOR_SERVER=127.0.0.1:<port> for WebKitWebDriver")
        })?
        .parse()?;
    anyhow::ensure!(
        address.ip().is_loopback() && address.port() != 0,
        "WebKit automation requires a loopback inspector address with a nonzero port"
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_is_opt_in_and_local_only() {
        assert!(!validate(None, None).unwrap());
        assert!(!validate(Some("0"), Some("0.0.0.0:9222")).unwrap());
        assert!(validate(Some("1"), Some("127.0.0.1:9222")).unwrap());
        assert!(validate(Some("1"), Some("[::1]:9222")).unwrap());
        for address in [
            None,
            Some("0.0.0.0:9222"),
            Some("192.0.2.1:9222"),
            Some("localhost:9222"),
            Some("127.0.0.1:0"),
            Some("bad"),
        ] {
            assert!(validate(Some("1"), address).is_err(), "{address:?}");
        }
        assert!(validate(Some("true"), Some("127.0.0.1:9222")).is_err());
    }
}
