// SPDX-License-Identifier: GPL-3.0-or-later
use super::super::browser_control::BrowserControl;
use flowmux_cookies::{cookie::SameSite, Cookie};
use objc2::{rc::Retained, runtime::AnyObject};
use objc2_foundation::{NSArray, NSDate, NSHTTPCookie, NSMutableDictionary, NSNumber, NSString};
use objc2_web_kit::WKHTTPCookieStore;

#[derive(Clone)]
pub(crate) struct Store(pub Retained<WKHTTPCookieStore>);

impl Store {
    async fn all(&self) -> Result<Vec<Retained<NSHTTPCookie>>, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = std::cell::RefCell::new(Some(tx));
        let callback =
            block2::RcBlock::new(move |cookies: std::ptr::NonNull<NSArray<NSHTTPCookie>>| {
                if let Some(tx) = tx.borrow_mut().take() {
                    let _ = tx.send(unsafe { cookies.as_ref() }.to_vec());
                }
            });
        unsafe {
            self.0.getAllCookies(&callback);
        }
        gtk::glib::future_with_timeout(std::time::Duration::from_secs(10), rx)
            .await
            .map_err(|_| "cookie store read timed out")?
            .map_err(|_| "cookie store callback closed".into())
    }

    pub async fn insert_missing(
        &self,
        cookies: &[Cookie],
        control: &BrowserControl,
        revision: u64,
    ) -> Result<(usize, usize), String> {
        if cookies.is_empty() {
            return Ok((0, 0));
        }
        let prepared = cookies.iter().map(native).collect::<Result<Vec<_>, _>>()?;
        let mut existing = self.all().await?;
        let mut added = Vec::new();
        let mut kept = 0;
        for cookie in prepared {
            control
                .check_revision(revision)
                .map_err(|e| format!("{e}; {} cookies already imported", added.len()))?;
            if existing
                .iter()
                .any(|old| identity(old) == identity(&cookie))
            {
                kept += 1;
                continue;
            }
            let (tx, rx) = tokio::sync::oneshot::channel();
            let tx = std::cell::RefCell::new(Some(tx));
            let done = block2::RcBlock::new(move || {
                if let Some(tx) = tx.borrow_mut().take() {
                    let _ = tx.send(());
                }
            });
            unsafe {
                self.0.setCookie_completionHandler(&cookie, Some(&done));
            }
            gtk::glib::future_with_timeout(std::time::Duration::from_secs(10), rx)
                .await
                .map_err(|_| {
                    format!(
                        "cookie write timed out after {} confirmed writes; inspect before retrying",
                        added.len()
                    )
                })?
                .map_err(|_| "cookie write callback closed")?;
            existing.push(cookie.clone());
            added.push(cookie);
        }
        let observed = self.all().await?;
        for cookie in &added {
            if !observed.iter().any(|c| {
                identity(c) == identity(cookie)
                    && c.value() == cookie.value()
                    && c.isSecure() == cookie.isSecure()
                    && c.isHTTPOnly() == cookie.isHTTPOnly()
                    && c.sameSitePolicy() == cookie.sameSitePolicy()
                    && c.expiresDate() == cookie.expiresDate()
                    && c.isSessionOnly() == cookie.isSessionOnly()
            }) {
                return Err(format!(
                    "cookie store did not retain all requested attributes; {} writes completed",
                    added.len()
                ));
            }
        }
        Ok((added.len(), kept))
    }
}

fn identity(cookie: &NSHTTPCookie) -> (String, String, String) {
    (
        cookie
            .domain()
            .to_string()
            .trim_start_matches('.')
            .to_ascii_lowercase(),
        cookie.path().to_string(),
        cookie.name().to_string(),
    )
}

fn native(cookie: &Cookie) -> Result<Retained<NSHTTPCookie>, String> {
    cookie.validate()?;
    let properties = NSMutableDictionary::<NSString, AnyObject>::new();
    for (key, value) in [
        ("Name", cookie.name.as_str()),
        ("Value", cookie.value.as_str()),
        ("Domain", cookie.host.as_str()),
        ("Path", cookie.path.as_str()),
    ] {
        properties.insert(&*NSString::from_str(key), &*NSString::from_str(value));
    }
    if cookie.secure {
        properties.insert(
            &*NSString::from_str("Secure"),
            &*NSNumber::numberWithBool(true),
        );
    }
    if cookie.http_only {
        properties.insert(
            &*NSString::from_str("HttpOnly"),
            &*NSNumber::numberWithBool(true),
        );
    }
    if let Some(expires) = cookie.expires_at {
        properties.insert(
            &*NSString::from_str("Expires"),
            &*NSDate::dateWithTimeIntervalSince1970(expires.timestamp() as f64),
        );
    }
    let policy = match cookie.same_site {
        SameSite::Lax => Some("lax"),
        SameSite::Strict => Some("strict"),
        SameSite::None => Some("none"),
        SameSite::NoRestriction => None,
    };
    if let Some(policy) = policy {
        properties.insert(
            &*NSString::from_str("SameSite"),
            &*NSString::from_str(policy),
        );
    }
    let parsed = unsafe { NSHTTPCookie::cookieWithProperties(&properties) }
        .ok_or("native cookie conversion failed")?;
    let actual_policy = parsed
        .sameSitePolicy()
        .map(|p| p.to_string().to_ascii_lowercase());
    let policy_matches = actual_policy.as_deref() == policy;
    for (preserved, attribute) in [
        (policy_matches, "SameSite"),
        (parsed.domain().to_string() == cookie.host, "domain"),
        (parsed.path().to_string() == cookie.path, "path"),
        (
            parsed
                .expiresDate()
                .map(|date| date.timeIntervalSince1970() as i64)
                == cookie.expires_at.map(|date| date.timestamp()),
            "expiry",
        ),
        (parsed.name().to_string() == cookie.name, "name"),
        (parsed.value().to_string() == cookie.value, "value"),
        (parsed.isHTTPOnly() == cookie.http_only, "HttpOnly"),
        (parsed.isSecure() == cookie.secure, "Secure"),
    ] {
        if !preserved {
            return Err(format!(
                "native cookie conversion cannot preserve {attribute}"
            ));
        }
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_conversion_keeps_insecure_session_cookies_insecure() {
        let cookie = Cookie {
            host: "127.0.0.1".into(),
            name: "pref".into(),
            value: "fixture".into(),
            path: "/".into(),
            expires_at: None,
            secure: false,
            http_only: false,
            same_site: SameSite::Lax,
        };
        let parsed = native(&cookie).unwrap();
        assert!(!parsed.isSecure() && !parsed.isHTTPOnly());
        assert!(parsed.isSessionOnly());
        let unsupported_expiry = Cookie {
            expires_at: Some(chrono::Utc::now() + chrono::Duration::days(1000)),
            ..cookie
        };
        assert!(
            native(&unsupported_expiry).is_err(),
            "Foundation clamps expiration to its maximum cookie lifetime"
        );
    }
    #[test]
    fn native_conversion_preserves_security_scope_and_expiry() {
        for policy in [
            SameSite::Lax,
            SameSite::Strict,
            SameSite::None,
            SameSite::NoRestriction,
        ] {
            for host in ["example.test", ".example.test"] {
                let cookie = Cookie {
                    host: host.into(),
                    name: "sid".into(),
                    value: "fixture".into(),
                    path: "/private".into(),
                    expires_at: Some(
                        chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp() + 3600, 0)
                            .unwrap(),
                    ),
                    secure: true,
                    http_only: true,
                    same_site: policy,
                };
                let parsed = native(&cookie).unwrap_or_else(|e| panic!("{host} {policy:?}: {e}"));
                assert!(parsed.isHTTPOnly() && parsed.isSecure());
                assert_eq!(parsed.domain().to_string(), host);
                assert!(!parsed.isSessionOnly());
            }
        }
    }
}
