// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::{command::SettingsOp, settings::SettingKey};

impl App {
    pub(super) fn settings_status(&self) -> Value {
        json!({"document":self.settings,"path":self.settings_worker.path,
            "persistent":self.settings_worker.path.is_some(),"config_error":self.settings_error,
            "pending_writes":self.settings_pending.len(),
            "surfaces":self.surfaces.iter().map(|(id,s)|json!({"surface":id,"applied":s.applied_settings})).collect::<Vec<_>>()})
    }
    pub(super) fn settings_submit(
        &mut self,
        op: SettingsOp,
        reply: Option<ipc::Reply>,
        editor: Option<Uuid>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.close_request.is_none(),
            "window is saving before close"
        );
        let id = Uuid::new_v4();
        self.settings_worker.submit(id, op)?;
        self.settings_pending.insert(id, (reply, editor));
        Ok(())
    }
    pub(super) fn settings_result(
        &mut self,
        update: super::super::settings_store::Update,
    ) -> anyhow::Result<()> {
        let pending = update
            .request
            .and_then(|id| self.settings_pending.remove(&id));
        let result = match update.result {
            Ok(document) => {
                self.settings_error = None;
                if self.settings != document {
                    self.settings = document;
                    for surface in self.surfaces.values_mut() {
                        surface.applied_settings = None;
                        if surface.ready || surface.restoring {
                            if let Err(error) = surface.send(&HostMessage::Settings {
                                document: self.settings.clone(),
                            }) {
                                report(&format!("settings delivery: {error:#}"));
                            }
                        }
                    }
                }
                Ok(())
            }
            Err(error) => {
                self.settings_error = Some(error.clone());
                Err(error)
            }
        };
        if let Some((reply, editor)) = pending {
            if let Some(reply) = reply {
                let value = match &result {
                    Ok(()) => self.settings_status(),
                    Err(error) => json!({"error":error}),
                };
                let _ = reply.try_send(value);
            }
            if let Some(editor) = editor {
                if let Some(panel) = self.metadata.as_ref().filter(|p| p.edit_id == editor) {
                    match &result {
                        Ok(()) => panel.hide(),
                        Err(error) => panel.status(error),
                    }
                }
            }
        }
        for control in &self.controls {
            if matches!(control.action, Action::Settings) {
                unsafe {
                    SetWindowTextW(
                        control.hwnd,
                        wide(if self.settings_error.is_some() {
                            "Settings (!)…"
                        } else {
                            "Settings…"
                        })
                        .as_ptr(),
                    );
                }
            }
        }
        Ok(())
    }
    pub(super) fn settings_menu(&mut self) -> anyhow::Result<()> {
        let value = &self.settings.terminal;
        let labels = vec![
            format!("Font family… ({})", value.font_family.replace('&', "&&")),
            format!("Font size… ({})", value.font_size),
            "Larger text".into(),
            "Smaller text".into(),
            "Reset text size".into(),
            "Dark terminal theme".into(),
            "Light terminal theme".into(),
            format!("Cursor blink: {} (toggle)", value.cursor_blink),
            "Block cursor".into(),
            "Underline cursor".into(),
            "Bar cursor".into(),
            format!("Scrollback… ({})", value.scrollback),
            "Reset settings (including default shell)".into(),
            self.settings_error
                .clone()
                .unwrap_or_else(|| "Settings apply to all flowmux windows".into())
                .replace('&', "&&"),
            "Default shell: Windows PowerShell".into(),
            "Default shell: Command Prompt".into(),
            "Default shell: PowerShell 7".into(),
            format!(
                "New tab with shell… (default: {})",
                self.settings.default_shell.program.replace('&', "&&")
            ),
        ];
        let mut rect = RECT::default();
        let hwnd = self
            .controls
            .iter()
            .find(|c| matches!(c.action, Action::Settings))
            .map_or(self.window, |c| c.hwnd);
        unsafe {
            GetWindowRect(hwnd, &mut rect);
        }
        let choice = self.popup(
            &labels.iter().map(String::as_str).collect::<Vec<_>>(),
            &[14],
            (rect.left, rect.bottom),
        )?;
        if (15..=17).contains(&choice) {
            self.settings_submit(
                SettingsOp::Shell {
                    program: ["powershell", "cmd", "pwsh"][choice - 15].into(),
                    args: vec![],
                },
                None,
                None,
            )?;
            return self.focus_active();
        }
        if choice == 18 {
            return self.shell_menu((rect.left, rect.bottom));
        }
        let edit = match choice {
            1 => Some(SettingKey::FontFamily),
            2 => Some(SettingKey::FontSize),
            12 => Some(SettingKey::Scrollback),
            _ => None,
        };
        if let Some(key) = edit {
            return self.edit_metadata(workspaces::EditTarget::Setting(key));
        }
        let changed = match choice {
            3 => (
                SettingKey::FontSize,
                (value.font_size + 1).min(72).to_string(),
            ),
            4 => (
                SettingKey::FontSize,
                value.font_size.saturating_sub(1).max(6).to_string(),
            ),
            5 => (SettingKey::FontSize, "14".into()),
            6 => (SettingKey::Theme, "dark".into()),
            7 => (SettingKey::Theme, "light".into()),
            8 => (SettingKey::CursorBlink, (!value.cursor_blink).to_string()),
            9 => (SettingKey::CursorStyle, "block".into()),
            10 => (SettingKey::CursorStyle, "underline".into()),
            11 => (SettingKey::CursorStyle, "bar".into()),
            13 => return self.settings_submit(SettingsOp::Reset, None, None),
            _ => return self.focus_active(),
        };
        self.settings_submit(
            SettingsOp::Set {
                key: changed.0,
                value: changed.1,
                expected: Some(value.value(changed.0)),
            },
            None,
            None,
        )?;
        self.focus_active()
    }
}
