// SPDX-License-Identifier: GPL-3.0-or-later
//! Executable and argv are separate. Stored terminal output is never a command.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shell {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}
impl Default for Shell {
    fn default() -> Self {
        Self::profile("powershell")
    }
}
impl Shell {
    pub fn profile(program: &str) -> Self {
        Self {
            program: program.into(),
            args: vec![],
        }
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.program.trim().is_empty()
                && !self.program.chars().any(char::is_control)
                && !self.program.contains('"')
                && self.program.encode_utf16().count() <= 32700,
            "shell must be an executable name or path, without quotes or control characters"
        );
        anyhow::ensure!(
            self.args.len() <= 64 && self.args.iter().all(|s| !s.contains('\0')),
            "shell arguments must contain no NUL; at most 64 arguments are allowed"
        );
        anyhow::ensure!(
            self.program.encode_utf16().count()
                + self
                    .args
                    .iter()
                    .map(|s| quote_arg(s).encode_utf16().count() + 1)
                    .sum::<usize>()
                < 32700,
            "shell command line exceeds the Windows limit"
        );
        Ok(())
    }
}

/// MS C runtime argv escaping; cmd /C and other command languages have their own
/// parsers. Callers must explicitly choose that interpreter if they need scripts.
pub fn quote_arg(value: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for ch in value.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        out.extend(std::iter::repeat_n(
            '\\',
            if ch == '"' { slashes * 2 + 1 } else { slashes },
        ));
        slashes = 0;
        out.push(ch);
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn executable_and_arguments_keep_unicode_and_escape_windows_argv() {
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("한글 한 😀 &"), "\"한글 한 😀 &\"");
        assert_eq!(quote_arg("a\\\"b\\"), "\"a\\\\\\\"b\\\\\"");
        assert!(Shell {
            program: "cmd\0evil".into(),
            args: vec![]
        }
        .validate()
        .is_err());
        assert!(Shell {
            program: "cmd".into(),
            args: vec!["\0".into()]
        }
        .validate()
        .is_err());
        assert!(Shell {
            program: "cmd".into(),
            args: vec!["x".repeat(32767)]
        }
        .validate()
        .is_err());
    }
}
