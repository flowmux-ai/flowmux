// SPDX-License-Identifier: GPL-3.0-or-later
use super::super::browser_control::BrowserControl;
use flowmux_cookies::{cookie::SameSite, Cookie};

#[derive(Clone)]
pub(crate) struct Store(pub webkit6::CookieManager);

impl Store {
    pub async fn insert_missing(
        &self,
        cookies: &[Cookie],
        control: &BrowserControl,
        revision: u64,
    ) -> Result<(usize, usize), String> {
        use webkit6::soup;
        if cookies.is_empty() {
            return Ok((0, 0));
        }
        let mut existing = self
            .0
            .all_cookies_future()
            .await
            .map_err(|e| e.to_string())?;
        let mut added = Vec::new();
        let mut kept = 0;
        for cookie in cookies {
            control
                .check_revision(revision)
                .map_err(|e| format!("{e}; {} cookies already imported", added.len()))?;
            cookie.validate()?;
            let mut native =
                soup::Cookie::new(&cookie.name, &cookie.value, &cookie.host, &cookie.path, -1);
            if existing
                .iter_mut()
                .any(|c| identity(c) == identity(&mut native))
            {
                kept += 1;
                continue;
            }
            native.set_secure(cookie.secure);
            native.set_http_only(cookie.http_only);
            native.set_same_site_policy(match cookie.same_site {
                SameSite::Strict => soup::SameSitePolicy::Strict,
                SameSite::Lax => soup::SameSitePolicy::Lax,
                _ => soup::SameSitePolicy::None,
            });
            if let Some(expires) = cookie.expires_at {
                native.set_expires(
                    &gtk::glib::DateTime::from_unix_utc(expires.timestamp())
                        .map_err(|e| e.to_string())?,
                );
            }
            self.0
                .add_cookie_future(&native)
                .await
                .map_err(|e| format!("{e}; {} cookies already imported", added.len()))?;
            existing.push(native.clone());
            added.push(native);
        }
        let mut observed = self
            .0
            .all_cookies_future()
            .await
            .map_err(|e| e.to_string())?;
        for cookie in &mut added {
            if !observed.iter_mut().any(|c| {
                identity(c) == identity(cookie)
                    && c.value() == cookie.value()
                    && c.is_secure() == cookie.is_secure()
                    && c.is_http_only() == cookie.is_http_only()
                    && c.same_site_policy() == cookie.same_site_policy()
                    && c.expires() == cookie.expires()
            }) {
                return Err(format!(
                    "cookie store verification failed after {} writes",
                    added.len()
                ));
            }
        }
        Ok((added.len(), kept))
    }
}
fn identity(
    c: &mut webkit6::soup::Cookie,
) -> (
    String,
    Option<gtk::glib::GString>,
    Option<gtk::glib::GString>,
) {
    (
        c.domain()
            .unwrap_or_default()
            .trim_start_matches('.')
            .to_ascii_lowercase(),
        c.path(),
        c.name(),
    )
}
