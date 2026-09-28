// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows shortcut validation using the shared action catalogue and defaults.
#[path = "../../crates/flowmux-config/src/keybindings.rs"]
mod shared;
pub use shared::{default_accels, defaults, ActionId, KeybindingOverrides};

use anyhow::{bail, ensure, Context};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chord {
    pub code: String,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub action: String,
    pub chord: Chord,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionBinding {
    pub action: String,
    pub label: String,
    pub supported: bool,
    pub defaults: Vec<String>,
    pub accels: Vec<String>,
    pub overridden: bool,
}

#[derive(Debug, Clone, Subcommand, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum Op {
    Show,
    /// Replace an action's shortcuts; no accelerators explicitly unbinds it.
    Set(SetArgs),
    /// Remove an override and restore this action's built-in shortcuts.
    Clear(ClearArgs),
    /// Restore all built-in shortcuts without changing other settings.
    Reset(ResetArgs),
}

#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct SetArgs {
    pub action: String,
    pub accels: Vec<String>,
    #[arg(skip)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<KeybindingOverrides>,
}
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct ClearArgs {
    pub action: String,
    #[arg(skip)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<KeybindingOverrides>,
}
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct ResetArgs {
    #[arg(skip)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<KeybindingOverrides>,
}

pub fn supported() -> &'static [ActionId] {
    use ActionId::*;
    &[
        SplitRight,
        SplitDown,
        FocusLeft,
        FocusRight,
        FocusUp,
        FocusDown,
        CloseSurface,
        QuitApp,
        NextSurface,
        PrevSurface,
        NextWorkspace,
        PrevWorkspace,
        Workspace1,
        Workspace2,
        Workspace3,
        Workspace4,
        Workspace5,
        Workspace6,
        Workspace7,
        Workspace8,
        NewSurface,
        NewBrowserSurface,
        NewWorkspace,
        TerminalSearch,
        SearchAllTerminals,
        TogglePaneZoom,
        ToggleWorkspaceOverview,
        ToggleFileBrowser,
    ]
}

/// Parse GTK accelerators or Ctrl+Shift+Key notation into physical DOM key codes.
/// Text, composition and AltGraph filtering remain the input dispatcher's job.
pub fn parse(accel: &str) -> anyhow::Result<Chord> {
    ensure!(accel.len() <= 128, "shortcut exceeds 128 bytes");
    let mut input = accel.trim();
    ensure!(!input.is_empty(), "shortcut is empty");
    let mut chord = Chord {
        code: String::new(),
        ctrl: false,
        alt: false,
        shift: false,
    };
    let key;
    if input.starts_with('<') {
        while input.starts_with('<') {
            let end = input.find('>').context("unclosed shortcut modifier")?;
            modifier(&mut chord, &input[1..end])?;
            input = &input[end + 1..];
        }
        key = input.trim();
    } else {
        let parts: Vec<_> = input.split('+').map(str::trim).collect();
        key = parts.last().copied().unwrap_or_default();
        for part in &parts[..parts.len().saturating_sub(1)] {
            modifier(&mut chord, part)?;
        }
    }
    chord.code = key_code(key).with_context(|| format!("unsupported shortcut key: {key}"))?;
    ensure!(
        chord.ctrl
            || chord.alt
            || chord
                .code
                .strip_prefix('F')
                .is_some_and(|s| s.parse::<u8>().is_ok()),
        "shortcuts require Ctrl or Alt, or a function key; text input is reserved"
    );
    ensure!(
        !reserved(&chord),
        "shortcut is reserved for clipboard or Windows: {accel}"
    );
    Ok(chord)
}

fn modifier(chord: &mut Chord, value: &str) -> anyhow::Result<()> {
    let flag = match value.to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "primary" => &mut chord.ctrl,
        "alt" => &mut chord.alt,
        "shift" => &mut chord.shift,
        "win" | "windows" | "super" | "meta" | "altgraph" => {
            bail!("Windows and AltGraph modifiers are reserved")
        }
        _ => bail!("unsupported shortcut modifier: {value}"),
    };
    ensure!(!*flag, "duplicate shortcut modifier: {value}");
    *flag = true;
    Ok(())
}

fn key_code(key: &str) -> Option<String> {
    let lower = key.to_ascii_lowercase();
    let letter = lower.strip_prefix("key").unwrap_or(&lower);
    if letter.len() == 1 && letter.as_bytes()[0].is_ascii_lowercase() {
        return Some(format!("Key{}", letter.to_ascii_uppercase()));
    }
    let digit = lower.strip_prefix("digit").unwrap_or(&lower);
    if digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit() {
        return Some(format!("Digit{digit}"));
    }
    if let Some(number) = lower.strip_prefix('f').and_then(|s| s.parse::<u8>().ok()) {
        if (1..=24).contains(&number) {
            return Some(format!("F{number}"));
        }
    }
    Some(
        match lower.as_str() {
            "left" | "arrowleft" => "ArrowLeft",
            "right" | "arrowright" => "ArrowRight",
            "up" | "arrowup" => "ArrowUp",
            "down" | "arrowdown" => "ArrowDown",
            "pageup" | "page_up" | "prior" => "PageUp",
            "pagedown" | "page_down" | "next" => "PageDown",
            "home" => "Home",
            "end" => "End",
            "insert" | "ins" => "Insert",
            "delete" | "del" => "Delete",
            "backspace" | "back_space" => "Backspace",
            "tab" => "Tab",
            "return" | "enter" => "Enter",
            "escape" | "esc" => "Escape",
            "space" => "Space",
            "minus" => "Minus",
            "equal" => "Equal",
            "bracketleft" => "BracketLeft",
            "bracketright" => "BracketRight",
            "backslash" => "Backslash",
            "semicolon" => "Semicolon",
            "quote" | "apostrophe" => "Quote",
            "backquote" | "grave" => "Backquote",
            "comma" => "Comma",
            "period" => "Period",
            "slash" => "Slash",
            _ => return None,
        }
        .into(),
    )
}

fn reserved(c: &Chord) -> bool {
    (c.ctrl && !c.alt && matches!(c.code.as_str(), "KeyC" | "KeyV" | "Insert"))
        || (!c.ctrl && !c.alt && c.shift && c.code == "Insert")
        || (c.alt && !c.ctrl && matches!(c.code.as_str(), "Tab" | "F4" | "Escape" | "Space"))
        || (c.ctrl && c.code == "Escape")
        || (c.ctrl && c.alt && c.code == "Delete")
}

pub fn resolved(overrides: &KeybindingOverrides) -> anyhow::Result<Vec<Binding>> {
    ensure!(
        overrides.unknown_keys().is_empty(),
        "unknown shortcut action"
    );
    for action in ActionId::all() {
        ensure!(
            supported().contains(action) || overrides.get(*action).is_none(),
            "shortcut action is unsupported on Windows: {}",
            action.as_str()
        );
    }
    let mut seen = HashMap::<Chord, &'static str>::new();
    let mut result = Vec::new();
    for action in supported() {
        let accels: Vec<_> = overrides
            .get(*action)
            .map(|a| a.iter().map(String::as_str).collect())
            .unwrap_or_else(|| default_accels(*action).to_vec());
        ensure!(accels.len() <= 8, "at most eight shortcuts per action");
        for accel in accels {
            let chord = parse(accel).with_context(|| format!("{}: {accel}", action.as_str()))?;
            if let Some(other) = seen.insert(chord.clone(), action.as_str()) {
                bail!(
                    "duplicate shortcut {accel}: {other} and {}",
                    action.as_str()
                );
            }
            result.push(Binding {
                action: action.as_str().into(),
                chord,
            });
        }
    }
    Ok(result)
}

pub fn validate(overrides: &KeybindingOverrides) -> anyhow::Result<()> {
    resolved(overrides).map(|_| ())
}

pub fn catalog(overrides: &KeybindingOverrides) -> anyhow::Result<Vec<ActionBinding>> {
    validate(overrides)?;
    Ok(ActionId::all()
        .iter()
        .map(|action| {
            let defaults: Vec<_> = default_accels(*action)
                .iter()
                .map(|s| (*s).into())
                .collect();
            ActionBinding {
                action: action.as_str().into(),
                label: action.label().into(),
                supported: supported().contains(action),
                accels: overrides
                    .get(*action)
                    .map(<[String]>::to_vec)
                    .unwrap_or_else(|| defaults.clone()),
                overridden: overrides.get(*action).is_some(),
                defaults,
            }
        })
        .collect())
}

pub fn change(current: &KeybindingOverrides, op: &Op) -> anyhow::Result<KeybindingOverrides> {
    let expected = match op {
        Op::Show => bail!("show does not write shortcuts"),
        Op::Set(args) => &args.expected,
        Op::Clear(args) => &args.expected,
        Op::Reset(args) => &args.expected,
    };
    ensure!(
        expected.as_ref().is_none_or(|value| value == current),
        "shortcuts changed elsewhere; reload values"
    );
    let mut next = current.clone();
    match op {
        Op::Set(args) => next.set(editable_action(&args.action)?, args.accels.clone()),
        Op::Clear(args) => next.clear(editable_action(&args.action)?),
        Op::Reset(_) => next.clear_all(),
        Op::Show => unreachable!(),
    }
    validate(&next)?;
    Ok(next)
}

fn editable_action(value: &str) -> anyhow::Result<ActionId> {
    let action = ActionId::from_wire(value).context("unknown shortcut action")?;
    ensure!(
        supported().contains(&action),
        "shortcut action is unsupported on Windows: {value}"
    );
    Ok(action)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_aliases_conflicts_reserved_and_unbind_are_validated() {
        let defaults = KeybindingOverrides::default();
        assert_eq!(resolved(&defaults).unwrap().len(), 28);
        assert_eq!(
            parse("<Ctrl><Shift>Page_Up").unwrap(),
            parse("Ctrl+Shift+PageUp").unwrap()
        );
        assert_eq!(
            parse("<Ctrl><Alt>k").unwrap(),
            parse("Control+Alt+KeyK").unwrap()
        );
        for invalid in [
            "Ctrl+C",
            "Ctrl+Shift+V",
            "Alt+F4",
            "Ctrl+Alt+Delete",
            "Win+K",
            "AltGraph+K",
            "Ctrl+Ctrl+K",
            "한",
            "K",
            "Ctrl+",
        ] {
            assert!(parse(invalid).is_err(), "{invalid}");
        }
        let set = |accels: Vec<&str>| {
            Op::Set(SetArgs {
                action: "toggle-workspace-overview".into(),
                accels: accels.into_iter().map(String::from).collect(),
                expected: Some(defaults.clone()),
            })
        };
        assert!(change(&defaults, &set(vec!["Ctrl+N"])).is_err());
        assert!(change(&defaults, &set(vec!["Ctrl+Alt+K", "<Ctrl><Alt>k"])).is_err());
        let unbound = change(&defaults, &set(vec![])).unwrap();
        assert_eq!(resolved(&unbound).unwrap().len(), 27);
        assert!(change(&unbound, &set(vec!["Ctrl+Alt+K"])).is_err());
        assert_eq!(
            change(
                &unbound,
                &Op::Clear(ClearArgs {
                    action: "toggle-workspace-overview".into(),
                    expected: Some(unbound.clone())
                })
            )
            .unwrap(),
            defaults
        );
    }
}
