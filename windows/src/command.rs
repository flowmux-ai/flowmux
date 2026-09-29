// SPDX-License-Identifier: GPL-3.0-or-later
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(name = "flowmux", about = "Open a native Windows flowmux window")]
pub struct Launch {
    #[command(flatten)]
    pub shell: ShellArgs,
    #[arg(long, conflicts_with = "restore_window")]
    pub cwd: Option<PathBuf>,
    #[arg(long, conflicts_with = "restore_window")]
    pub new_window: bool,
    /// Open without reading or saving persistent window state.
    #[arg(long, conflicts_with = "restore_window")]
    pub temporary: bool,
    #[arg(long, conflicts_with_all = ["shell", "shell_args"])]
    pub restore_window: Option<Uuid>,
}

/// One grammar for both GUI and console entry points. Do not classify an
/// invocation using its first argument: options can use `=` or change order.
#[derive(Debug, Parser)]
#[command(
    name = "flowmux",
    version,
    about = "Open or control native Windows flowmux"
)]
struct Entry {
    #[command(flatten)]
    launch: Launch,
    #[arg(long, global = true)]
    pipe: Option<String>,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug)]
pub enum Invocation {
    Launch { options: Launch, json: bool },
    Client(Cli),
}

/// Validate an already classified window launch without building every CLI
/// subcommand on the native UI thread's deeper event-handler stack.
pub fn validate_launch(
    arguments: impl IntoIterator<Item = impl Into<std::ffi::OsString> + Clone>,
) -> Result<(), clap::Error> {
    Launch::command()
        .arg(
            clap::Arg::new("json")
                .long("json")
                .action(clap::ArgAction::SetTrue),
        )
        .try_get_matches_from(arguments)
        .map(|_| ())
}

pub fn parse_entry(
    arguments: impl IntoIterator<Item = impl Into<std::ffi::OsString> + Clone>,
) -> Result<Invocation, clap::Error> {
    let entry = Entry::try_parse_from(arguments)?;
    if let Some(command) = entry.command {
        let launch = &entry.launch;
        if launch.cwd.is_some()
            || launch.new_window
            || launch.temporary
            || launch.restore_window.is_some()
            || launch.shell.shell.is_some()
            || !launch.shell.shell_args.is_empty()
        {
            return Err(Entry::command().error(
                clap::error::ErrorKind::ArgumentConflict,
                "window launch options cannot be combined with a CLI command",
            ));
        }
        Ok(Invocation::Client(Cli {
            pipe: entry.pipe,
            json: entry.json,
            command,
        }))
    } else if entry.pipe.is_some() {
        Err(Entry::command().error(
            clap::error::ErrorKind::MissingSubcommand,
            "--pipe requires a CLI command",
        ))
    } else {
        Ok(Invocation::Launch {
            options: entry.launch,
            json: entry.json,
        })
    }
}

#[derive(Debug, Clone, Default, clap::Args, Serialize, Deserialize)]
pub struct ShellArgs {
    /// Built-in powershell/cmd/pwsh profile, absolute executable, or PATH name.
    #[arg(long)]
    #[serde(default)]
    pub shell: Option<String>,
    /// One argv item; repeat for multiple arguments. No command-string splitting.
    #[arg(long = "shell-arg", allow_hyphen_values = true, requires = "shell")]
    #[serde(default)]
    pub shell_args: Vec<String>,
}
impl ShellArgs {
    pub fn requested(&self) -> anyhow::Result<Option<crate::shell::Shell>> {
        anyhow::ensure!(
            self.shell.is_some() || self.shell_args.is_empty(),
            "shell arguments require --shell"
        );
        self.shell
            .as_ref()
            .map(|program| {
                let shell = crate::shell::Shell {
                    program: program.clone(),
                    args: self.shell_args.clone(),
                };
                shell.validate()?;
                Ok(shell)
            })
            .transpose()
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "flowmuxctl",
    version,
    about = "Control a native Windows flowmux window"
)]
pub struct Cli {
    #[arg(long, global = true)]
    pub pipe: Option<String>,
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    #[serde(flatten)]
    pub command: Command,
    /// The surface remains stable when its inherited pane/workspace IDs become stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_surface: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_cwd: Option<PathBuf>,
}

// Keep the unoptimized clap grammar builder below the Windows main-stack limit.
#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
pub struct NotifyArgs {
    #[arg(long, default_value = "Terminal")]
    pub title: String,
    #[arg(long, default_value="info", value_parser=["info","attention","error","completed"])]
    pub level: String,
    pub body: String,
    #[arg(long, value_parser=parse_id, conflicts_with="surface")]
    pub pane: Option<Uuid>,
    #[arg(long, value_parser=parse_id)]
    pub surface: Option<Uuid>,
    /// Create an entry without a terminal source.
    #[arg(long, conflicts_with_all=["pane","surface"])]
    #[serde(default)]
    pub global: bool,
}

#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
pub struct FindArgs {
    #[arg(required_unless_present = "close", conflicts_with = "close")]
    pub query: Option<String>,
    #[arg(long, value_parser = parse_id)]
    pub surface: Option<Uuid>,
    #[arg(long, conflicts_with = "close")]
    #[serde(default)]
    pub previous: bool,
    #[arg(long, conflicts_with = "close")]
    #[serde(default)]
    pub match_case: bool,
    #[arg(long, conflicts_with = "close")]
    #[serde(default)]
    pub regex: bool,
    #[arg(long)]
    #[serde(default)]
    pub close: bool,
}

#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
pub struct NotifyCompleteArgs {
    #[arg(long)]
    pub agent: String,
    #[arg(long, default_value = "task complete")]
    pub message: String,
    #[arg(long, value_parser=parse_id, conflicts_with="surface")]
    pub pane: Option<Uuid>,
    #[arg(long, value_parser=parse_id)]
    pub surface: Option<Uuid>,
}

#[derive(Debug, Clone, Subcommand, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Command {
    /// Internal launcher request, never exposed as a shell command argument.
    #[command(skip)]
    LaunchWindow {
        context: Box<crate::window_launch::LaunchContext>,
    },
    /// Check local Windows and WebView2 prerequisites without connecting to a window.
    Doctor,
    #[cfg(debug_assertions)]
    #[command(hide = true)]
    ChromeCapture {
        path: PathBuf,
        #[arg(long)]
        #[serde(default)]
        usage_bar: bool,
    },
    #[cfg(debug_assertions)]
    #[command(hide = true)]
    TestShortcut {
        #[arg(value_parser = parse_id)]
        surface: Uuid,
        event: String,
    },
    #[cfg(debug_assertions)]
    #[command(hide = true)]
    TestTerminalMenu {
        #[arg(value_parser = parse_id)]
        surface: Uuid,
        event: String,
    },
    #[cfg(debug_assertions)]
    #[command(hide = true)]
    TestUsage {
        input: String,
    },
    /// Print the session-local PowerShell prompt integration for manual reinstallation.
    ShellIntegration,
    Identify,
    Capabilities,
    /// Discover native built-in shell profiles without launching them.
    Shells,
    /// Retry a failed terminal startup. A running or exited process is never replaced.
    RetryShell {
        #[arg(long, value_parser = parse_id)]
        surface: Option<Uuid>,
        #[command(flatten)]
        #[serde(flatten)]
        shell: ShellArgs,
    },
    /// Show or update the Windows terminal appearance shared by windows.
    Settings {
        #[command(subcommand)]
        op: SettingsOp,
    },
    Tree,
    /// List live local agents in this window's owned terminal processes.
    Agents,
    /// Control an in-app browser pane, separate from terminal content.
    Browser {
        #[command(subcommand)]
        op: crate::browser::Op,
    },
    /// Open and control a local Monaco editor tab.
    Editor {
        #[command(subcommand)]
        op: crate::editor::Op,
    },
    /// Browse local files in a native panel without leaving the current pane.
    Files {
        #[command(subcommand)]
        op: crate::files_model::Op,
    },
    /// Add an in-app notification. Desktop delivery is not yet implemented.
    Notify(NotifyArgs),
    /// Report an agent turn completion without inferring agent identity/state.
    NotifyComplete(NotifyCompleteArgs),
    Downloads {
        #[command(subcommand)]
        op: crate::downloads::Op,
    },
    Notifications {
        #[command(subcommand)]
        op: crate::notifications::Op,
    },
    /// Inspect or navigate a terminal minimap without changing keyboard focus.
    Minimap {
        #[arg(long, value_parser = parse_id)]
        surface: Option<Uuid>,
        #[command(subcommand)]
        action: crate::minimap::Action,
    },
    /// Read parsed physical rows without changing focus, selection or scrolling.
    #[command(visible_alias = "capture-pane")]
    #[serde(alias = "capture_pane")]
    ReadScreen {
        #[arg(value_parser = parse_id)]
        pane: Option<Uuid>,
        /// Read a specific tab, including an inactive tab, without changing focus.
        #[arg(long, value_parser = parse_id, conflicts_with = "pane")]
        surface: Option<Uuid>,
        /// Read the latest 80 normal-buffer rows, or the entire alternate screen.
        #[arg(long)]
        #[serde(default)]
        recent: bool,
    },
    /// Find in a terminal's retained output without activating an inactive tab.
    Find(FindArgs),
    /// Start a paged literal search across all terminals in this window.
    SearchAll {
        query: String,
        #[arg(long)]
        #[serde(default)]
        match_case: bool,
        #[arg(long, default_value_t = 0)]
        #[serde(default)]
        offset: usize,
    },
    /// Poll a search, including its matching lines and unavailable terminals.
    SearchResults {
        search: Uuid,
    },
    /// Cancel a search and invalidate its retained result references.
    SearchCancel {
        search: Uuid,
    },
    /// Select a result from the current page after verifying its retained text.
    SearchOpen {
        search: Uuid,
        index: usize,
    },
    /// Inspect or change a terminal selection without accessing the OS clipboard.
    Selection {
        #[arg(long, value_parser = parse_id)]
        surface: Option<Uuid>,
        #[command(subcommand)]
        action: crate::selection::Action,
    },
    /// Paste explicit text using the terminal's current bracketed-paste mode.
    /// Does not access the system clipboard. Success means queued, not consumed.
    Paste {
        text: String,
        #[arg(long, value_parser = parse_id, conflicts_with = "surface")]
        pane: Option<Uuid>,
        /// Target an inactive tab without changing focus.
        #[arg(long, value_parser = parse_id)]
        surface: Option<Uuid>,
    },
    SendKeys {
        #[arg(value_parser = parse_id)]
        pane: Uuid,
        text: String,
    },
    /// Send one terminal key after parsing the current cursor mode.
    // Old hosts ignore unknown fields. A new wire method makes them reject a
    // new --surface request instead of silently typing into the active tab.
    #[serde(rename = "send_key_mode", alias = "send_key")]
    SendKey {
        key: String,
        #[arg(long, value_parser = parse_id, conflicts_with = "surface")]
        pane: Option<Uuid>,
        /// Target an inactive tab without changing focus.
        #[arg(long, value_parser = parse_id)]
        surface: Option<Uuid>,
    },
    Split {
        #[arg(value_enum, default_value = "vertical")]
        direction: Direction,
        #[command(flatten)]
        #[serde(flatten)]
        shell: ShellArgs,
    },
    NewTab {
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[command(flatten)]
        #[serde(flatten)]
        shell: ShellArgs,
    },
    NewWorkspace {
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[command(flatten)]
        #[serde(flatten)]
        shell: ShellArgs,
    },
    /// List, focus, rename, color, reorder or close a workspace in this window.
    Workspace {
        #[command(subcommand)]
        op: WorkspaceOp,
    },
    /// Set a tab's user-locked title without activating or restarting it.
    RenameTab {
        #[arg(value_parser = parse_id)]
        surface: Uuid,
        name: String,
    },
    FocusPane {
        #[arg(value_parser = parse_id)]
        pane: Uuid,
    },
    /// Resize a split or the immediate parent of a pane (ratio of the first child).
    ResizePane {
        #[arg(value_parser = parse_id)]
        pane: Uuid,
        #[arg(long, allow_hyphen_values = true)]
        ratio: f32,
    },
    /// Focus the nearest pane in a direction within the source workspace.
    FocusDirection {
        #[arg(value_enum)]
        direction: FocusDirection,
        #[arg(long, value_parser = parse_id)]
        pane: Option<Uuid>,
    },
    /// Maximize a pane, or restore its existing split layout.
    TogglePaneZoom {
        #[arg(value_parser = parse_id)]
        pane: Option<Uuid>,
    },
    FocusTab {
        #[arg(value_parser = parse_id)]
        surface: Uuid,
    },
    CloseTab {
        #[arg(value_parser = parse_id)]
        surface: Uuid,
    },
    /// Close every tab in a pane; refuses the workspace's final pane.
    ClosePane {
        #[arg(value_parser = parse_id)]
        pane: Uuid,
    },
    /// Move a running tab without restarting its process or terminal view.
    MoveTab {
        #[arg(value_parser = parse_id)]
        surface: Uuid,
        #[arg(long, value_parser = parse_id)]
        to_pane: Uuid,
        /// Zero-based destination position; omitted means append.
        #[arg(long)]
        index: Option<usize>,
    },
    /// Move an existing terminal tab into its own window without restarting it.
    DetachTab {
        #[arg(value_parser = parse_id)]
        surface: Uuid,
    },
    /// Close this Windows window and terminate its terminal process trees.
    Quit {
        /// Close even when saving fails; keep the last completed checkpoint.
        #[arg(long)]
        #[serde(default)]
        discard_state: bool,
    },
    /// Save layout and styled terminal history without closing the window.
    SaveState,
}

#[derive(Debug, Clone, Subcommand, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SettingsOp {
    Show,
    /// Show or customize supported Windows shortcuts.
    Keybindings {
        #[command(subcommand)]
        op: crate::keybindings::Op,
    },
    /// Set the shell for future tabs/workspaces. Existing terminals keep their shell.
    Shell {
        program: String,
        #[arg(long = "arg", allow_hyphen_values = true)]
        #[serde(default)]
        args: Vec<String>,
        // Options compares the complete shell under the settings writer lock.
        #[arg(skip)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected: Option<crate::shell::Shell>,
    },
    Set {
        #[arg(value_enum)]
        key: crate::settings::SettingKey,
        value: String,
        /// Refuse to replace a value changed by another window.
        #[arg(long)]
        #[serde(default)]
        expected: Option<String>,
    },
    /// Restore terminal appearance defaults, including an invalid settings file.
    Reset,
}

#[derive(Debug, Clone, Copy, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Vertical,
    Horizontal,
}

#[derive(Debug, Clone, Copy, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Subcommand, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum WorkspaceOp {
    List,
    Current,
    Focus {
        #[arg(value_parser = parse_id)]
        workspace: Uuid,
    },
    Rename {
        #[arg(value_parser = parse_id)]
        workspace: Uuid,
        name: String,
    },
    /// Use #RRGGBB; omit it with --clear to remove the color.
    Color {
        #[arg(value_parser = parse_id)]
        workspace: Uuid,
        #[arg(required_unless_present = "clear", conflicts_with = "clear")]
        color: Option<String>,
        #[arg(long)]
        #[serde(default)]
        clear: bool,
    },
    /// Move to a zero-based absolute position, retaining the active workspace.
    Reorder {
        #[arg(value_parser = parse_id)]
        workspace: Uuid,
        index: usize,
    },
    /// Terminate every terminal in this workspace. The final workspace is protected.
    Close {
        #[arg(value_parser = parse_id)]
        workspace: Uuid,
    },
}

pub(crate) fn parse_id(text: &str) -> Result<Uuid, String> {
    let value = text
        .strip_prefix("pane:")
        .or_else(|| text.strip_prefix("surface:"))
        .or_else(|| text.strip_prefix("workspace:"))
        .unwrap_or(text);
    Uuid::parse_str(value).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extracted_command_args_preserve_cli_and_flat_tagged_wire() {
        let id = Uuid::new_v4().to_string();
        for (args, expected) in [
            (
                vec!["detach-tab", &id],
                serde_json::json!({"method":"detach_tab","surface":id}),
            ),
            (
                vec![
                    "notify",
                    "--title",
                    "한글",
                    "--level",
                    "attention",
                    "body",
                    "--surface",
                    &id,
                ],
                serde_json::json!({"method":"notify","title":"한글","level":"attention","body":"body","pane":null,"surface":id,"global":false}),
            ),
            (
                vec![
                    "notify-complete",
                    "--agent",
                    "worker",
                    "--message",
                    "done",
                    "--pane",
                    &id,
                ],
                serde_json::json!({"method":"notify_complete","agent":"worker","message":"done","pane":id,"surface":null}),
            ),
            (
                vec![
                    "find",
                    "한글",
                    "--surface",
                    &id,
                    "--previous",
                    "--match-case",
                    "--regex",
                ],
                serde_json::json!({"method":"find","query":"한글","surface":id,"previous":true,"match_case":true,"regex":true,"close":false}),
            ),
            (
                vec!["settings", "shell", "cmd", "--arg", "/d"],
                serde_json::json!({"method":"settings","op":{"action":"shell","program":"cmd","args":["/d"]}}),
            ),
        ] {
            let cli = Cli::try_parse_from(std::iter::once("flowmuxctl").chain(args)).unwrap();
            let value = serde_json::to_value(&cli.command).unwrap();
            assert_eq!(value, expected);
            let decoded: Command = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), value);
        }
    }
    #[test]
    fn browser_commands_roundtrip_pane_context_and_reject_unknown_options() {
        let id = Uuid::new_v4();
        let pane = format!("pane:{id}");
        for args in [
            vec![
                "browser",
                "open",
                "https://example.com/한글",
                "--pane",
                &pane,
                "--down",
            ],
            vec!["browser", "navigate", &pane, "about:blank"],
            vec!["browser", "status", &pane],
            vec!["browser", "eval", &pane, "document.title"],
        ] {
            let cli = Cli::try_parse_from(std::iter::once("flowmuxctl").chain(args)).unwrap();
            let request = Request {
                command: cli.command,
                caller_surface: None,
                caller_cwd: None,
            };
            let value = serde_json::to_value(&request).unwrap();
            let decoded: Request = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), value);
        }
        assert!(Cli::try_parse_from([
            "flowmuxctl",
            "browser",
            "open",
            "about:blank",
            "--right",
            "--down"
        ])
        .is_err());
        assert!(serde_json::from_value::<Request>(serde_json::json!({"method":"browser","op":{"kind":"status","pane":id,"unexpected":true}})).is_err());
    }
    #[test]
    fn send_key_preserves_old_wire_shape_and_adds_explicit_surface_routing() {
        let old: Request =
            serde_json::from_str(r#"{"method":"send_key","key":"Enter","pane":null}"#).unwrap();
        assert!(matches!(
            old.command,
            Command::SendKey { surface: None, .. }
        ));
        let id = Uuid::new_v4().to_string();
        let cli =
            Cli::try_parse_from(["flowmuxctl", "send-key", "Ctrl+Z", "--surface", &id]).unwrap();
        assert_eq!(
            serde_json::to_value(&cli.command).unwrap()["method"],
            "send_key_mode"
        );
        assert!(matches!(
            cli.command,
            Command::SendKey {
                surface: Some(_),
                pane: None,
                ..
            }
        ));
        assert!(Cli::try_parse_from([
            "flowmuxctl",
            "send-key",
            "Enter",
            "--surface",
            &id,
            "--pane",
            &id
        ])
        .is_err());
    }

    #[test]
    fn notification_cli_and_wire_keep_source_and_operation_unambiguous() {
        let id = Uuid::new_v4().to_string();
        let cli = Cli::try_parse_from([
            "flowmuxctl",
            "notify",
            "--surface",
            &id,
            "--title",
            "한글",
            "--level",
            "attention",
            "한 😀",
        ])
        .unwrap();
        let request = Request {
            command: cli.command,
            caller_surface: None,
            caller_cwd: None,
        };
        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(wire["title"], "한글");
        assert_eq!(wire["body"], "한 😀");
        assert_eq!(wire["surface"], id);
        let decoded: Request = serde_json::from_value(wire).unwrap();
        assert!(matches!(
            decoded.command,
            Command::Notify(NotifyArgs { global: false, .. })
        ));
        assert!(
            Cli::try_parse_from(["flowmuxctl", "notify", "--global", "--surface", &id, "x"])
                .is_err()
        );
        assert!(Cli::try_parse_from(["flowmuxctl", "notify", "--level", "urgent", "x"]).is_err());
        let cli = Cli::try_parse_from(["flowmuxctl", "notifications", "list", "--unread"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Notifications {
                op: crate::notifications::Op::List { unread: true }
            }
        ));
        assert!(serde_json::from_str::<Request>(
            r#"{"method":"notifications","op":{"kind":"clear","id":"unexpected"}}"#
        )
        .is_err());
    }

    #[test]
    fn capture_alias_and_recent_reads_preserve_targeting_and_legacy_wire_requests() {
        let id = Uuid::new_v4().to_string();
        for verb in ["read-screen", "capture-pane"] {
            let cli =
                Cli::try_parse_from(["flowmuxctl", verb, "--surface", &id, "--recent"]).unwrap();
            assert!(matches!(
                cli.command,
                Command::ReadScreen {
                    pane: None,
                    surface: Some(_),
                    recent: true
                }
            ));
            assert!(Cli::try_parse_from(["flowmuxctl", verb, &id, "--surface", &id]).is_err());
            assert!(matches!(
                parse_entry(["flowmux", verb, &id]).unwrap(),
                Invocation::Client(_)
            ));
        }
        for method in ["read_screen", "capture_pane"] {
            let request: Request =
                serde_json::from_value(serde_json::json!({"method":method})).unwrap();
            assert!(matches!(
                request.command,
                Command::ReadScreen { recent: false, .. }
            ));
        }
    }
    #[test]
    fn unified_entry_parses_launch_options_and_commands_without_first_argument_routing() {
        for args in [
            vec!["flowmux"],
            vec!["flowmux", "--shell", "cmd", "--temporary"],
            vec!["flowmux", "--shell=cmd", "--temporary"],
            vec!["flowmux", "--cwd=C:\\한글 한 😀", "--json", "--shell=cmd"],
            vec!["flowmux", "--json", "--new-window"],
            vec!["flowmux", "--shell=cmd", "--shell-arg", "--json"],
            vec![
                "flowmux",
                "--restore-window=00000000-0000-0000-0000-000000000000",
            ],
        ] {
            assert!(validate_launch(&args).is_ok());
            assert!(matches!(
                parse_entry(args).unwrap(),
                Invocation::Launch { .. }
            ));
        }
        for args in [
            vec!["flowmux", "--json", "tree", "--pipe", "example"],
            vec!["flowmux", "--pipe=example", "tree", "--json"],
            vec!["flowmux", "new-tab", "--shell=cmd", "--cwd=C:\\한글"],
        ] {
            assert!(validate_launch(&args).is_err());
            assert!(matches!(parse_entry(args).unwrap(), Invocation::Client(_)));
        }
        for args in [
            vec!["flowmux", "--shell=cmd", "tree"],
            vec!["flowmux", "--temporary", "tree"],
            vec!["flowmux", "--pipe=example"],
            vec!["flowmux", "--shell-arg=orphan"],
            vec!["flowmux", "unknown-command"],
            vec![
                "flowmux",
                "--restore-window=00000000-0000-0000-0000-000000000000",
                "--new-window",
            ],
        ] {
            assert!(validate_launch(&args).is_err());
            assert_eq!(parse_entry(args).unwrap_err().exit_code(), 2);
        }
        for args in [
            vec!["flowmux", "--help"],
            vec!["flowmux", "--version"],
            vec!["flowmux", "help", "new-tab"],
        ] {
            assert_eq!(parse_entry(args).unwrap_err().exit_code(), 0);
        }
        let help = parse_entry(["flowmux", "--help"]).unwrap_err().to_string();
        assert!(help.contains("--shell") && help.contains("read-screen"));
    }
    #[test]
    fn shell_cli_and_legacy_requests_keep_each_argv_item_separate() {
        let launch = Launch::try_parse_from(["flowmux", "--shell", "cmd"]).unwrap();
        assert_eq!(launch.shell.requested().unwrap().unwrap().program, "cmd");
        assert!(Launch::try_parse_from([
            "flowmux",
            "--restore-window",
            "00000000-0000-0000-0000-000000000000",
            "--shell",
            "cmd"
        ])
        .is_err());
        let cli = Cli::try_parse_from([
            "flowmuxctl",
            "new-tab",
            "--shell",
            "custom.exe",
            "--shell-arg",
            "",
            "--shell-arg",
            "한글 & quote\"\\",
        ])
        .unwrap();
        let Command::NewTab { shell, .. } = cli.command else {
            panic!()
        };
        assert_eq!(
            shell.requested().unwrap().unwrap().args,
            vec!["", "한글 & quote\"\\"]
        );
        assert!(Cli::try_parse_from(["flowmuxctl", "new-tab", "--shell-arg", "x"]).is_err());
        for json in [
            r#"{"method":"new_tab"}"#,
            r#"{"method":"split","direction":"vertical"}"#,
            r#"{"method":"new_workspace"}"#,
        ] {
            let request: Request = serde_json::from_str(json).unwrap();
            let shell = match request.command {
                Command::NewTab { shell, .. }
                | Command::Split { shell, .. }
                | Command::NewWorkspace { shell, .. } => shell,
                _ => panic!(),
            };
            assert!(shell.requested().unwrap().is_none());
        }
    }

    #[test]
    fn context_preserves_old_ipc_commands_and_roundtrips_stable_surface() {
        let old: Request = serde_json::from_str(r#"{"method":"identify"}"#).unwrap();
        assert!(old.caller_surface.is_none());
        assert!(old.caller_cwd.is_none());
        let old_quit: Request = serde_json::from_str(r#"{"method":"quit"}"#).unwrap();
        assert!(matches!(
            old_quit.command,
            Command::Quit {
                discard_state: false
            }
        ));
        let old_read: Request =
            serde_json::from_str(r#"{"method":"read_screen","pane":null}"#).unwrap();
        assert!(matches!(
            old_read.command,
            Command::ReadScreen {
                pane: None,
                surface: None,
                recent: false
            }
        ));
        let id = Uuid::new_v4();
        let original = Request {
            command: Command::ReadScreen {
                pane: None,
                surface: None,
                recent: false,
            },
            caller_surface: Some(id),
            caller_cwd: Some("C:\\한글 folder".into()),
        };
        let decoded: Request =
            serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
        assert_eq!(decoded.caller_surface, Some(id));
        assert_eq!(decoded.caller_cwd, original.caller_cwd);
        assert!(matches!(
            decoded.command,
            Command::ReadScreen {
                pane: None,
                surface: None,
                recent: false
            }
        ));
    }
}
