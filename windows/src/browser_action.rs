// SPDX-License-Identifier: GPL-3.0-or-later
//! Ref-based DOM operations; these do not inject Windows input.
use crate::browser::Op;
use anyhow::ensure;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// Shared argument structs keep clap's unoptimized enum builder below the
// Windows main-thread stack limit while preserving the CLI and JSON grammar.
#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetArgs {
    #[arg(value_parser = crate::command::parse_id)]
    pub pane: Uuid,
    pub target: String,
}
#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValueArgs {
    #[arg(value_parser = crate::command::parse_id)]
    pub pane: Uuid,
    pub target: String,
    pub value: String,
}
#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScrollArgs {
    #[arg(value_parser = crate::command::parse_id)]
    pub pane: Uuid,
    pub target: String,
    #[arg(allow_hyphen_values = true)]
    pub x: i32,
    #[arg(allow_hyphen_values = true)]
    pub y: i32,
}

#[derive(Debug, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Action {
    Click,
    Dblclick,
    Hover,
    Focus,
    Blur,
    Scroll { x: i32, y: i32 },
    Fill { value: String },
    Select { value: String },
    Check,
    Uncheck,
}
impl Action {
    pub fn script(&self, mut args: Value) -> anyhow::Result<String> {
        if let Self::Fill { value } | Self::Select { value } = self {
            ensure!(
                value.len() <= 64 * 1024,
                "browser action value exceeds 64 KiB"
            );
        }
        for (key, value) in serde_json::to_value(self)?.as_object().unwrap() {
            args[key] = value.clone();
        }
        let script = format!("({})({args})", include_str!("../browser/action.js"));
        ensure!(
            script.len() <= 128 * 1024,
            "encoded browser action exceeds 128 KiB"
        );
        Ok(script)
    }
    pub fn allowed_in_background(&self) -> bool {
        !matches!(self, Self::Focus | Self::Blur)
    }
}
pub fn from_op(op: &Op) -> Option<(&str, Action)> {
    Some(match op {
        Op::Click(args) => (&args.target, Action::Click),
        Op::Dblclick(args) => (&args.target, Action::Dblclick),
        Op::Hover(args) => (&args.target, Action::Hover),
        Op::Focus(args) => (&args.target, Action::Focus),
        Op::Blur(args) => (&args.target, Action::Blur),
        Op::Scroll(args) => (
            &args.target,
            Action::Scroll {
                x: args.x,
                y: args.y,
            },
        ),
        Op::Fill(args) => (
            &args.target,
            Action::Fill {
                value: args.value.clone(),
            },
        ),
        Op::Select(args) => (
            &args.target,
            Action::Select {
                value: args.value.clone(),
            },
        ),
        Op::Check(args) => (&args.target, Action::Check),
        Op::Uncheck(args) => (&args.target, Action::Uncheck),
        _ => return None,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn action_data_is_unicode_exact_bounded_and_background_focus_is_rejected() {
        let value = "한글 한 é 😀 \" \\ \n');window.injected=true;//";
        let script = Action::Fill {
            value: value.into(),
        }
        .script(serde_json::json!({}))
        .unwrap();
        assert!(script.contains(&serde_json::to_string(value).unwrap()));
        for value in ["a".repeat(65537), "\0".repeat(40000)] {
            assert!(Action::Fill { value }
                .script(serde_json::json!({}))
                .is_err());
        }
        assert!(!Action::Focus.allowed_in_background());
        assert!(!Action::Blur.allowed_in_background());
        assert!(Action::Fill {
            value: String::new()
        }
        .allowed_in_background());
    }
}
