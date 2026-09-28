// SPDX-License-Identifier: GPL-3.0-or-later
//! Preserve valid RFC 8187 UTF-8 filename* bytes before browser normalization.
//! Malformed/duplicate parameters and other charsets use the browser fallback.
pub fn extended(header: &str) -> Option<String> {
    if header.len() > 16384 || header.contains(['\r', '\n', '\0']) {
        return None;
    }
    let (mut quoted, mut escaped) = (false, false);
    let mut parts = Vec::new();
    let mut start = 0;
    for (index, ch) in header.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ';' if !quoted => {
                parts.push(&header[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    if quoted || escaped {
        return None;
    }
    parts.push(&header[start..]);
    let mut value = None;
    for part in parts.into_iter().skip(1) {
        let Some((key, data)) = part.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("filename*") {
            if value.is_some() {
                return None;
            }
            value = Some(data.trim());
        }
    }
    let mut pieces = value?.splitn(3, '\'');
    if !pieces.next()?.eq_ignore_ascii_case("utf-8") {
        return None;
    }
    let language = pieces.next()?;
    if !language
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return None;
    }
    let data = pieces.next()?.as_bytes();
    let mut bytes = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == b'%' {
            let high = (*data.get(i + 1)? as char).to_digit(16)?;
            let low = (*data.get(i + 2)? as char).to_digit(16)?;
            bytes.push((high * 16 + low) as u8);
            i += 3;
        } else {
            let b = data[i];
            if !b.is_ascii_alphanumeric() && !b"!#$&+-.^_`|~".contains(&b) {
                return None;
            }
            bytes.push(b);
            i += 1;
        }
    }
    let text = String::from_utf8(bytes).ok()?;
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extended_filename_retains_exact_unicode_and_literal_plus() {
        let raw="attachment; filename=\"semi;colon.txt\"; FILENAME*=UTF-8'ko'%ED%95%9C%20%E1%84%92%E1%85%A1%E1%86%AB%20e%CC%81%20%F0%9F%98%80+.txt";
        assert_eq!(extended(raw).as_deref(), Some("한 한 é 😀+.txt"));
        assert_eq!(
            extended("attachment; filename*=UTF-8''a%3Bb.txt").as_deref(),
            Some("a;b.txt")
        );
        for raw in [
            "attachment; filename*=UTF-8''x; filename*=UTF-8''y",
            "attachment; filename*=ISO-8859-1''x",
            "attachment; filename*=UTF-8''%FF",
            "attachment; filename*=UTF-8''%A",
            "attachment; filename*=UTF-8''a b",
            "attachment; filename*=\"UTF-8''a\"",
            "attachment; filename=\"x; filename*=UTF-8''a",
            "attachment; filename*=UTF-8''",
        ] {
            assert!(extended(raw).is_none(), "{raw}");
        }
    }
}
