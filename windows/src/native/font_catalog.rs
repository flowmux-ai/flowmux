// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded GDI font discovery for a worker; no UI or global input access.
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{GetLastError, LPARAM},
    Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, EnumFontFamiliesExW, DEFAULT_CHARSET, HDC, LOGFONTW,
        SYMBOL_CHARSET, TEXTMETRICW, TMPF_FIXED_PITCH,
    },
};

const MAX_VISITS: u32 = 4096;
const MAX_FAMILIES: usize = 256;
const BUDGET: Duration = Duration::from_millis(300);
// Same ranking/prefix rule as Linux ui/options_dialog.rs, followed by Windows faces.
const CURATED: &[&str] = &[
    "JetBrains Mono",
    "Fira Code",
    "Cascadia Code",
    "Cascadia Mono",
    "Maple Mono",
    "Monaspace",
    "Geist Mono",
    "Commit Mono",
    "Source Code Pro",
    "Hack",
    "IBM Plex Mono",
    "Iosevka",
    "Intel One Mono",
    "0xProto",
    "Recursive Mono",
    "Inconsolata",
    "Roboto Mono",
    "Ubuntu Mono",
    "Red Hat Mono",
    "Victor Mono",
    "Fantasque Sans Mono",
    "Hasklig",
    "Mononoki",
    "Hermit",
    "Martian Mono",
    "Spline Sans Mono",
    "Overpass Mono",
    "B612 Mono",
    "Azeret Mono",
    "Anonymous Pro",
    "Space Mono",
    "Go Mono",
    "Departure Mono",
    "Sometype Mono",
    "Sarasa Mono",
    "Sarasa Term",
    "Sarasa Fixed",
    "D2Coding",
    "Nanum Gothic Coding",
    "NanumGothicCoding",
    "Noto Sans Mono",
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Cousine",
    "Fira Mono",
    "PT Mono",
    "Consolas",
    "Lucida Console",
    "Courier New",
];

pub(super) struct Catalog {
    pub families: Vec<String>,
    pub truncated: bool,
    pub visited: u32,
    pub elapsed_ms: u64,
}

struct Discovery {
    started: Instant,
    names: BTreeMap<String, String>,
    visited: u32,
    truncated: bool,
}
impl Discovery {
    fn reached_limit(&mut self) -> bool {
        if self.visited >= MAX_VISITS
            || self.names.len() >= MAX_FAMILIES
            || self.started.elapsed() >= BUDGET
        {
            self.truncated = true;
            true
        } else {
            false
        }
    }
}
struct MemoryDc(HDC);
impl Drop for MemoryDc {
    fn drop(&mut self) {
        unsafe {
            DeleteDC(self.0);
        }
    }
}

fn rank(lower: &str) -> Option<usize> {
    CURATED.iter().position(|base| {
        let base = base.to_ascii_lowercase();
        lower == base
            || lower
                .strip_prefix(&base)
                .is_some_and(|suffix| suffix.starts_with(' '))
    })
}

fn consider(names: &mut BTreeMap<String, String>, name: String) {
    if name.trim().is_empty() || name.starts_with('@') || name.chars().any(char::is_control) {
        return;
    }
    let lower = name.to_lowercase();
    if rank(&lower).is_some() {
        // Retain the installed spelling, including localized Unicode codepoints.
        names.entry(lower).or_insert(name);
    }
}

unsafe extern "system" fn visit(
    font: *const LOGFONTW,
    metric: *const TEXTMETRICW,
    _font_type: u32,
    context: LPARAM,
) -> i32 {
    if context == 0 || font.is_null() || metric.is_null() {
        return 0;
    }
    // EnumFontFamiliesExW calls synchronously on this worker. This context and
    // its HDC remain owned by enumerate until the native call returns.
    let state = &mut *(context as *mut Discovery);
    if state.reached_limit() {
        return 0;
    }
    state.visited += 1;
    let metric = &*metric;
    // GDI's misleading flag is SET for variable pitch, CLEAR for fixed pitch.
    if metric.tmPitchAndFamily & TMPF_FIXED_PITCH == 0 && metric.tmCharSet != SYMBOL_CHARSET {
        let face = &(*font).lfFaceName;
        let length = face
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(face.len());
        if let Ok(name) = String::from_utf16(&face[..length]) {
            consider(&mut state.names, name);
        }
    }
    i32::from(!state.reached_limit())
}

/// Cooperative callback limits do not interrupt a stalled native GDI call.
/// The caller must keep its UI deadline independent and must not join on UI teardown.
pub(super) fn enumerate() -> Result<Catalog, String> {
    let started = Instant::now();
    let dc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
    if dc.is_null() {
        return Err(format!("could not create font discovery DC: {}", unsafe {
            GetLastError()
        }));
    }
    let dc = MemoryDc(dc);
    let mut state = Discovery {
        started,
        names: BTreeMap::new(),
        visited: 0,
        truncated: false,
    };
    let query = LOGFONTW {
        lfCharSet: DEFAULT_CHARSET,
        ..Default::default()
    };
    unsafe {
        // lfPitchAndFamily, empty face name and dwFlags must remain zero.
        // Return zero also means our callback stopped; it is not a Win32 error code.
        EnumFontFamiliesExW(
            dc.0,
            &query,
            Some(visit),
            (&mut state as *mut Discovery) as LPARAM,
            0,
        );
    }
    let mut names: Vec<_> = state.names.into_iter().collect();
    names.sort_by(|(a, _), (b, _)| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
    Ok(Catalog {
        families: names.into_iter().map(|(_, name)| name).collect(),
        truncated: state.truncated,
        visited: state.visited,
        elapsed_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
    })
}

/// Replace only the primary family; never normalize or reconstruct the CSS tail.
/// This recognizes list boundaries, not the complete CSS grammar. Comments and
/// functional expressions require direct editing rather than an ambiguous rewrite.
pub(super) fn replace_primary(current: &str, family: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        !family.trim().is_empty() && !family.chars().any(char::is_control),
        "font family is empty or contains control characters"
    );
    let mut quoted = None;
    let mut escaped = false;
    let mut comma = None;
    let mut segment = 0;
    for (index, ch) in current.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(quote) = quoted {
            if ch == quote {
                quoted = None;
            }
        } else if matches!(ch, '\'' | '"') {
            quoted = Some(ch);
        } else if ch == ',' {
            nonempty_family(&current[segment..index])?;
            comma.get_or_insert(index);
            segment = index + 1;
        } else {
            anyhow::ensure!(
                !current[index..].starts_with("/*") && !matches!(ch, '(' | ')' | ';' | '{' | '}'),
                "existing font list uses unsupported CSS syntax; edit it directly"
            );
        }
    }
    anyhow::ensure!(
        quoted.is_none() && !escaped,
        "existing font list has an unfinished quote or escape; edit it directly"
    );
    nonempty_family(&current[segment..])?;
    let primary = current[..comma.unwrap_or(current.len())].trim();
    crate::settings::TerminalSettings::default().changed(
        crate::settings::SettingKey::FontFamily,
        current,
        None,
    )?;
    if plain_family(primary).is_some_and(|name| name.to_lowercase() == family.to_lowercase()) {
        return Ok(current.into());
    }
    let selected = format!("\"{}\"", family.replace('\\', "\\\\").replace('"', "\\\""));
    let next = if let Some(comma) = comma {
        format!("{selected}{}", &current[comma..])
    } else if primary.eq_ignore_ascii_case("monospace") {
        format!("{selected}, {current}")
    } else {
        format!("{selected}, monospace")
    };
    // Reuse the existing font-family UTF-16 limit and control-character checks.
    crate::settings::TerminalSettings::default().changed(
        crate::settings::SettingKey::FontFamily,
        &next,
        None,
    )?;
    Ok(next)
}

fn nonempty_family(value: &str) -> anyhow::Result<()> {
    let value = value.trim();
    let content = if (value.starts_with('"') && value.ends_with('"'))
        || (value.starts_with('\'') && value.ends_with('\''))
    {
        value
            .get(1..value.len().saturating_sub(1))
            .unwrap_or_default()
    } else {
        value
    };
    anyhow::ensure!(
        !content.trim().is_empty(),
        "existing font list has an empty family"
    );
    Ok(())
}

fn plain_family(value: &str) -> Option<String> {
    let quoted = value.starts_with('"') || value.starts_with('\'');
    let body = if quoted {
        value.get(1..value.len().checked_sub(1)?)?
    } else {
        value
    };
    let mut result = String::new();
    let mut chars = body.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let escaped = chars.next()?;
            // CSS hex escapes need full CSS parsing; do not guess their meaning.
            if escaped.is_ascii_hexdigit() {
                return None;
            }
            result.push(escaped);
        } else {
            result.push(ch);
        }
    }
    Some(if quoted {
        result
    } else {
        result
            .split_ascii_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn font_catalog_contract() {
        assert_eq!(
            replace_primary(r#""Old, Family",  "Malgun Gothic", monospace"#, "D2Coding").unwrap(),
            r#""D2Coding",  "Malgun Gothic", monospace"#
        );
        assert_eq!(
            replace_primary(r"Old\,Family,  '한글 한', monospace", "Fira Code").unwrap(),
            "\"Fira Code\",  '한글 한', monospace"
        );
        assert_eq!(
            replace_primary("Consolas", "A\\B\"C").unwrap(),
            r#""A\\B\"C", monospace"#
        );
        assert_eq!(
            replace_primary("  monospace  ", "Consolas").unwrap(),
            "\"Consolas\",   monospace  "
        );
        for (current, family) in [
            ("  'Consolas' ,  '한글', monospace", "consolas"),
            (r"Old\,Family, monospace", "Old,Family"),
            ("Consolas", "Consolas"),
        ] {
            assert_eq!(replace_primary(current, family).unwrap(), current);
        }
        assert_eq!(
            replace_primary("'Old/*,Name',  monospace", "Consolas").unwrap(),
            "\"Consolas\",  monospace"
        );
        for invalid in [
            "",
            " , monospace",
            "\"\", monospace",
            "'unfinished",
            "Old, 'bad",
            "Old, tail\\",
            "Old,",
            "Old,,monospace",
            "Old, ' '",
            "Old /* comment,comma */, monospace",
            "Old, var(--fallback, monospace)",
        ] {
            assert!(replace_primary(invalid, "Consolas").is_err(), "{invalid}");
        }
        assert!(replace_primary("Old", &"한".repeat(256)).is_err());
        let mut names = BTreeMap::new();
        for name in [
            "Consolas",
            "Fira Code Retina",
            "JetBrains Mono",
            "consolas",
            "Fira Code",
            "D2Coding",
            "@Consolas",
            "Arial",
            "Fira Codec",
        ] {
            consider(&mut names, name.into());
        }
        let mut names: Vec<_> = names.into_iter().collect();
        names.sort_by(|(a, _), (b, _)| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
        assert_eq!(
            names.into_iter().map(|(_, name)| name).collect::<Vec<_>>(),
            [
                "JetBrains Mono",
                "Fira Code",
                "Fira Code Retina",
                "D2Coding",
                "Consolas"
            ]
        );
    }
}
