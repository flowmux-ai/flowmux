// SPDX-License-Identifier: GPL-3.0-or-later

//! The slash commands Flowmux supplies for chat agents.
//!
//! Keep agent command changes here. Where the lists come from:
//!
//! - **Claude** reports its commands and skills in the `initialize` response
//!   (`claude::boot`). New Claude Code releases need no change here.
//! - **ACP agents** (OpenCode, Gemini, Cline, Antigravity) report theirs in
//!   `available_commands_update` (`acp::update_commands`). No change needed here either.
//! - **Codex** app-server reports only skills (`skills/list`, in `codex::boot`). A typed slash
//!   command reaches the model as text, so every Codex command needs a row in [`CODEX`] and
//!   an app-server request in `codex::send`. Leave out commands app-server cannot run.
//!
//! To add a Codex command: add a [`Run`] variant, add a row to [`CODEX`], and handle the
//! variant in `codex::send`. `/goal` is left out: it starts autonomous turns Flowmux does not
//! request.

/// How Flowmux runs a Codex command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Run {
    /// `thread/compact/start`; the turn ends with `thread/compacted` or `turn/completed`.
    Compact,
    /// `review/start` with inline delivery; arguments become custom instructions.
    Review,
}

pub(super) struct Builtin {
    pub name: &'static str,
    pub description: &'static str,
    /// Arguments shown after the name in the menu; empty for none.
    pub hint: &'static str,
    pub run: Run,
}

pub(super) const CODEX: &[Builtin] = &[
    Builtin {
        name: "compact",
        description: "Summarize the conversation to free context",
        hint: "",
        run: Run::Compact,
    },
    Builtin {
        name: "review",
        description: "Review uncommitted changes, or follow your instructions",
        hint: "[instructions]",
        run: Run::Review,
    },
];

/// The Codex command `text` invokes, with its arguments.
pub(super) fn codex(text: &str) -> Option<(Run, &str)> {
    let rest = text.trim_start().strip_prefix('/')?;
    CODEX.iter().find_map(|command| {
        let args = rest.strip_prefix(command.name)?;
        (args.is_empty() || args.starts_with(char::is_whitespace))
            .then(|| (command.run, args.trim()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_need_their_exact_name() {
        assert_eq!(codex("/compact"), Some((Run::Compact, "")));
        assert_eq!(
            codex("/review focus on tests"),
            Some((Run::Review, "focus on tests"))
        );
        assert_eq!(codex("/compaction"), None);
        assert_eq!(codex("please /compact"), None);
    }

    #[test]
    fn names_are_unique_menu_names() {
        for (index, command) in CODEX.iter().enumerate() {
            assert!(command
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '-'));
            assert!(CODEX[..index]
                .iter()
                .all(|other| other.name != command.name));
        }
    }
}
