// SPDX-License-Identifier: GPL-3.0-or-later

//! Colors code blocks: comments, strings, numbers, keywords, type names and diff lines.

// ponytail: a hand-rolled lexer without per-language grammars (no nesting, heredocs or
// string interpolation); switch to a grammar-based highlighter if a language needs that.

use adw::prelude::*;

const LIMIT: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Comment,
    String,
    Number,
    Keyword,
    Type,
    Added,
    Removed,
    Hunk,
    Meta,
}

impl Kind {
    const ALL: [Self; 9] = [
        Self::Comment,
        Self::String,
        Self::Number,
        Self::Keyword,
        Self::Type,
        Self::Added,
        Self::Removed,
        Self::Hunk,
        Self::Meta,
    ];

    fn tag(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::String => "string",
            Self::Number => "number",
            Self::Keyword => "keyword",
            Self::Type => "type",
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Hunk => "hunk",
            Self::Meta => "meta",
        }
    }

    /// GitHub-like (light, dark) foregrounds.
    fn colors(self) -> (&'static str, &'static str) {
        match self {
            Self::Comment => ("#6e7781", "#8b949e"),
            Self::String => ("#0a3069", "#a5d6ff"),
            Self::Number => ("#0550ae", "#79c0ff"),
            Self::Keyword => ("#cf222e", "#ff7b72"),
            Self::Type | Self::Hunk => ("#8250df", "#d2a8ff"),
            Self::Added => ("#116329", "#7ee787"),
            Self::Removed => ("#82071e", "#ffa198"),
            Self::Meta => ("#57606a", "#c9d1d9"),
        }
    }
}

struct Lang {
    line: &'static [&'static str],
    block: Option<(&'static str, &'static str)>,
    quotes: &'static str,
    keywords: &'static [&'static str],
    types: bool,
}

const C_LIKE: &[&str] = &[
    "break",
    "case",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "else",
    "enum",
    "extends",
    "false",
    "for",
    "if",
    "import",
    "in",
    "interface",
    "new",
    "null",
    "private",
    "protected",
    "public",
    "return",
    "static",
    "struct",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "catch",
    "finally",
    "typedef",
    "void",
    "while",
    "package",
    "func",
    "go",
    "var",
    "let",
    "fun",
    "val",
    "override",
    "namespace",
    "using",
    "auto",
    "sizeof",
    "template",
    "typename",
    "virtual",
    "async",
    "await",
    "yield",
    "export",
    "from",
    "function",
    "typeof",
    "instanceof",
    "of",
    "undefined",
    "as",
    "type",
    "implements",
    "readonly",
    "abstract",
    "final",
    "nil",
    "range",
    "defer",
    "chan",
    "select",
    "map",
    "guard",
    "self",
    "super",
    "where",
    "protocol",
    "extension",
];

const RUST: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];

const PYTHON: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "False", "finally", "for", "from", "global", "if", "import", "in", "is",
    "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "True", "try", "while",
    "with", "yield", "self",
];

const SHELL: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "in", "function", "return", "local", "export", "set", "unset", "readonly", "exit", "source",
];

const SQL: &[&str] = &[
    "select", "from", "where", "insert", "into", "values", "update", "set", "delete", "create",
    "table", "drop", "alter", "join", "left", "right", "inner", "outer", "on", "group", "by",
    "order", "having", "limit", "and", "or", "not", "null", "as", "distinct", "union", "index",
    "primary", "key", "SELECT", "FROM", "WHERE", "INSERT", "INTO", "VALUES", "UPDATE", "SET",
    "DELETE", "CREATE", "TABLE", "DROP", "ALTER", "JOIN", "LEFT", "RIGHT", "INNER", "ON", "GROUP",
    "BY", "ORDER", "HAVING", "LIMIT", "AND", "OR", "NOT", "NULL", "AS", "DISTINCT",
];

const DATA: &[&str] = &["true", "false", "null", "yes", "no", "on", "off"];

fn lang(name: &str) -> Option<Lang> {
    let name = name.to_ascii_lowercase();
    let c_like = |types| Lang {
        line: &["//"],
        block: Some(("/*", "*/")),
        quotes: "\"'`",
        keywords: C_LIKE,
        types,
    };
    Some(match name.as_str() {
        "rust" | "rs" => Lang {
            line: &["//"],
            block: Some(("/*", "*/")),
            // Lifetimes share the single quote, so only double quotes start strings.
            quotes: "\"",
            keywords: RUST,
            types: true,
        },
        "c" | "h" | "cpp" | "c++" | "cc" | "hpp" | "cxx" | "java" | "kotlin" | "kt" | "swift"
        | "cs" | "csharp" | "go" | "golang" | "scala" | "dart" | "php" | "js" | "javascript"
        | "jsx" | "mjs" | "cjs" | "ts" | "typescript" | "tsx" => c_like(true),
        "css" | "scss" => Lang {
            line: &[],
            block: Some(("/*", "*/")),
            quotes: "\"'",
            keywords: &[],
            types: false,
        },
        "python" | "py" => Lang {
            line: &["#"],
            block: None,
            quotes: "\"'",
            keywords: PYTHON,
            types: true,
        },
        "sh" | "bash" | "zsh" | "shell" | "console" | "shellsession" | "fish" => Lang {
            line: &["#"],
            block: None,
            quotes: "\"'",
            keywords: SHELL,
            types: false,
        },
        "ruby" | "rb" | "perl" | "pl" | "r" | "make" | "makefile" | "dockerfile" | "cmake" => {
            Lang {
                line: &["#"],
                block: None,
                quotes: "\"'",
                keywords: C_LIKE,
                types: false,
            }
        }
        "toml" | "yaml" | "yml" | "ini" | "conf" => Lang {
            line: &["#", ";"],
            block: None,
            quotes: "\"'",
            keywords: DATA,
            types: false,
        },
        "json" | "jsonc" | "json5" => Lang {
            line: &["//"],
            block: Some(("/*", "*/")),
            quotes: "\"",
            keywords: DATA,
            types: false,
        },
        "sql" | "lua" | "haskell" | "hs" => Lang {
            line: &["--"],
            block: Some(("/*", "*/")),
            quotes: "\"'",
            keywords: SQL,
            types: false,
        },
        _ => return None,
    })
}

/// Character ranges to color, in order.
pub(super) fn spans(language: &str, code: &str) -> Vec<(Kind, usize, usize)> {
    if code.len() > LIMIT {
        return Vec::new();
    }
    if matches!(language.to_ascii_lowercase().as_str(), "diff" | "patch") {
        return diff(code);
    }
    let Some(lang) = lang(language) else {
        return Vec::new();
    };
    let text: Vec<char> = code.chars().collect();
    let at = |i: usize, token: &str| {
        token
            .chars()
            .enumerate()
            .all(|(n, c)| text.get(i + n) == Some(&c))
    };
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut spans = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let start = i;
        let c = text[i];
        if lang.line.iter().any(|prefix| at(i, prefix)) {
            while i < text.len() && text[i] != '\n' {
                i += 1;
            }
            spans.push((Kind::Comment, start, i));
        } else if let Some((open, close)) = lang.block.filter(|(open, _)| at(i, open)) {
            i += open.chars().count();
            while i < text.len() && !at(i, close) {
                i += 1;
            }
            i = (i + close.chars().count()).min(text.len());
            spans.push((Kind::Comment, start, i));
        } else if lang.quotes.contains(c) {
            i += 1;
            while i < text.len() && text[i] != c && (c == '`' || text[i] != '\n') {
                i += if text[i] == '\\' { 2 } else { 1 };
            }
            i = (i + 1).min(text.len());
            spans.push((Kind::String, start, i));
        } else if c.is_ascii_digit() && (i == 0 || !word(text[i - 1])) {
            while i < text.len() && (word(text[i]) || text[i] == '.') {
                i += 1;
            }
            spans.push((Kind::Number, start, i));
        } else if word(c) && (i == 0 || !word(text[i - 1])) {
            while i < text.len() && word(text[i]) {
                i += 1;
            }
            let ident: String = text[start..i].iter().collect();
            if lang.keywords.contains(&ident.as_str()) {
                spans.push((Kind::Keyword, start, i));
            } else if lang.types && c.is_uppercase() && ident.chars().any(char::is_lowercase) {
                spans.push((Kind::Type, start, i));
            }
        } else {
            i += 1;
        }
    }
    spans
}

fn diff(code: &str) -> Vec<(Kind, usize, usize)> {
    let mut spans = Vec::new();
    let mut offset = 0;
    for line in code.split_inclusive('\n') {
        let len = line.chars().count();
        let kind = if line.starts_with("+++") || line.starts_with("---") {
            Some(Kind::Meta)
        } else if line.starts_with('+') {
            Some(Kind::Added)
        } else if line.starts_with('-') {
            Some(Kind::Removed)
        } else if line.starts_with("@@") {
            Some(Kind::Hunk)
        } else {
            None
        };
        if let Some(kind) = kind {
            spans.push((kind, offset, offset + len));
        }
        offset += len;
    }
    spans
}

thread_local! {
    static TAGS: gtk::TextTagTable = tag_table();
}

fn tag_table() -> gtk::TextTagTable {
    let table = gtk::TextTagTable::new();
    for kind in Kind::ALL {
        let tag = gtk::TextTag::new(Some(kind.tag()));
        match kind {
            Kind::Comment => tag.set_style(gtk::pango::Style::Italic),
            Kind::Meta => tag.set_weight(600),
            _ => {}
        }
        table.add(&tag);
    }
    let manager = adw::StyleManager::default();
    paint(&table, manager.is_dark());
    let weak = table.downgrade();
    manager.connect_dark_notify(move |manager| {
        if let Some(table) = weak.upgrade() {
            paint(&table, manager.is_dark());
        }
    });
    table
}

fn paint(table: &gtk::TextTagTable, dark: bool) {
    for kind in Kind::ALL {
        let Some(tag) = table.lookup(kind.tag()) else {
            continue;
        };
        let (light, night) = kind.colors();
        tag.set_foreground(Some(if dark { night } else { light }));
        let background = match kind {
            Kind::Added => Some(gtk::gdk::RGBA::new(0.18, 0.63, 0.26, 0.14)),
            Kind::Removed => Some(gtk::gdk::RGBA::new(0.85, 0.21, 0.2, 0.14)),
            _ => None,
        };
        tag.set_paragraph_background_rgba(background.as_ref());
    }
}

/// A buffer holding `code` with its colors; the tags follow light and dark styles.
pub(super) fn buffer(language: &str, code: &str) -> gtk::TextBuffer {
    let buffer = TAGS.with(|table| gtk::TextBuffer::new(Some(table)));
    buffer.set_text(code);
    for (kind, start, end) in spans(language, code) {
        buffer.apply_tag_by_name(
            kind.tag(),
            &buffer.iter_at_offset(start as i32),
            &buffer.iter_at_offset(end as i32),
        );
    }
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colored(language: &str, code: &str) -> Vec<(Kind, String)> {
        let chars: Vec<char> = code.chars().collect();
        spans(language, code)
            .into_iter()
            .map(|(kind, start, end)| (kind, chars[start..end].iter().collect()))
            .collect()
    }

    #[test]
    fn colors_tokens_without_touching_lifetimes_or_identifiers() {
        let rust = colored(
            "rust",
            "fn main<'a>() { let s: String = \"hi // no\"; 42 } // done",
        );
        assert_eq!(
            rust,
            [
                (Kind::Keyword, "fn".into()),
                (Kind::Keyword, "let".into()),
                (Kind::Type, "String".into()),
                (Kind::String, "\"hi // no\"".into()),
                (Kind::Number, "42".into()),
                (Kind::Comment, "// done".into()),
            ]
        );
        let python = colored("py", "def f(x):\n    return 'a\\'b'  # 한글 comment");
        assert_eq!(python[0], (Kind::Keyword, "def".into()));
        assert_eq!(python[2], (Kind::String, "'a\\'b'".into()));
        assert_eq!(python[3], (Kind::Comment, "# 한글 comment".into()));
        assert!(colored("unknown", "fn x() {}").is_empty());
    }

    #[test]
    fn colors_whole_diff_lines() {
        let kinds: Vec<_> = colored("diff", "--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new\n ctx\n")
            .into_iter()
            .map(|(kind, _)| kind)
            .collect();
        assert_eq!(
            kinds,
            [
                Kind::Meta,
                Kind::Meta,
                Kind::Hunk,
                Kind::Removed,
                Kind::Added
            ]
        );
    }
}
