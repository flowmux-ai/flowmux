// SPDX-License-Identifier: GPL-3.0-or-later
//! Download command grammar and Windows filename policy.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, clap::Subcommand, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    List {},
    Show {},
    Cancel {
        id: Uuid,
    },
    /// Forget a finished entry; its downloaded file is kept.
    Remove {
        id: Uuid,
    },
    /// Forget finished entries; downloaded files are kept.
    Clear {},
}

pub const MAX_ACTIVE: usize = 8;
pub const MAX_ROWS: usize = 50;

fn clipped(value: &str, units: usize) -> String {
    let mut used = 0;
    value
        .chars()
        .take_while(|ch| {
            used += ch.len_utf16();
            used <= units
        })
        .collect()
}
pub fn filename(value: &str) -> String {
    let leaf = value.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = leaf
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let clean = clean.trim_end_matches([' ', '.']);
    let (stem, ext) = split_extension(clean);
    let stem = clipped(stem, 180);
    let mut clean = match ext {
        Some(ext) => format!("{stem}.{}", clipped(ext, 32)),
        None => stem,
    };
    // Clipping can expose a trailing space/dot, an empty name, or a device
    // name that the unabridged input did not contain.
    clean.truncate(clean.trim_end_matches([' ', '.']).len());
    if clean.is_empty() {
        clean = "download".into();
    }
    let stem = clean
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|p| {
        stem.strip_prefix(p).is_some_and(|s| {
            matches!(
                s,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    }) {
        let (stem, ext) = split_extension(&clean);
        let stem = clipped(stem, 179);
        clean = match ext {
            Some(ext) => format!("_{stem}.{ext}"),
            None => format!("_{stem}"),
        };
        clean.truncate(clean.trim_end_matches([' ', '.']).len());
    }
    clean
}
/// Respect the browser-selected extension while recovering a Unicode name
/// from a valid UTF-8 extended disposition parameter.
pub fn suggested(header: &str, fallback: &str) -> String {
    let fallback = filename(fallback);
    if let Some(raw) = crate::download_name::extended(header) {
        let exact = filename(&raw);
        let left = split_extension(&exact).1.unwrap_or("");
        let right = split_extension(&fallback).1.unwrap_or("");
        if left.eq_ignore_ascii_case(right) {
            return exact;
        }
    }
    fallback
}
fn split_extension(name: &str) -> (&str, Option<&str>) {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, Some(ext)),
        _ => (name, None),
    }
}
pub fn candidate(name: &str, index: usize) -> String {
    if index == 0 {
        return name.into();
    }
    let (stem, ext) = split_extension(name);
    match ext {
        Some(ext) => format!("{stem} ({index}).{ext}"),
        None => format!("{stem} ({index})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_names_survive_without_normalization_or_path_traversal() {
        assert_eq!(
            filename("C:\\Downloads\\한 글 한 é 😀.txt"),
            "한 글 한 é 😀.txt"
        );
        assert_ne!(filename("한.txt"), filename("한.txt"));
        assert_eq!(filename("../../dir/a:b?.txt"), "a_b_.txt");
        assert_eq!(filename(".."), "download");
        assert_eq!(filename("NUL.txt"), "_NUL.txt");
        assert_eq!(filename("com¹.txt"), "_com¹.txt");
        assert_eq!(filename("trailing. "), "trailing");
        assert_eq!(filename(".gitignore"), ".gitignore");
        assert_eq!(
            suggested(
                "attachment; filename*=UTF-8''%E1%84%92%E1%85%A1%E1%86%AB.txt",
                "한.txt"
            ),
            "한.txt"
        );
        assert_eq!(
            suggested("attachment; filename*=UTF-8''name.exe", "name.txt"),
            "name.txt"
        );
        assert_eq!(candidate("한글.tar.gz", 2), "한글.tar (2).gz");
        let long = filename(&format!("{}.txt", "😀".repeat(1000)));
        assert!(long.encode_utf16().count() <= 213);
        assert!(long.ends_with(".txt"));
    }

    #[test]
    fn clipping_cannot_reintroduce_invalid_windows_names() {
        assert_eq!(filename(&format!("{} b", "a".repeat(179))), "a".repeat(179));
        assert_eq!(filename(&format!("report.{}txt", " ".repeat(32))), "report");
        assert_eq!(filename(&format!("{}visible", " ".repeat(180))), "download");
        assert_eq!(filename(&format!("CON{}tail", " ".repeat(177))), "_CON");
        assert_eq!(filename(&format!("LPT1{}tail", " ".repeat(176))), "_LPT1");
        let reserved_stem = filename(&format!("CON{}tail.txt", " ".repeat(177)));
        assert_eq!(reserved_stem, format!("_CON{}.txt", " ".repeat(176)));
        assert_eq!(
            split_extension(&reserved_stem).0.encode_utf16().count(),
            180
        );
    }
}
