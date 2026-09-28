// SPDX-License-Identifier: GPL-3.0-or-later
//! Preset rows and effective-color previews; writes stay in the Options worker.
use super::*;

pub(super) const PRESET: usize = 10;
pub(super) const FIRST_COLOR: usize = 11;
pub(super) const OVERRIDES: usize = 16;
pub(super) const ROW_COUNT: usize = 17;
const PRESET_BUTTON: usize = 7000;
const PICK_BUTTON: usize = 7100;
const SWATCH_BUTTON: usize = 7200;
const LEGACY: usize = 7300;
const RESET: usize = 7301;
#[derive(Clone, Copy)]
pub(crate) enum Signal {
    Preset(usize),
    Legacy,
    Pick(usize),
    Reset,
    Reveal(usize),
}
pub(super) fn command(id: usize, code: u32) -> Option<Signal> {
    if (PRESET_BUTTON..PRESET_BUTTON + 11).contains(&id) {
        return match code {
            BN_CLICKED => Some(Signal::Preset(id - PRESET_BUTTON)),
            BN_SETFOCUS => Some(Signal::Reveal(id - PRESET_BUTTON)),
            _ => None,
        };
    }
    if code != BN_CLICKED {
        return None;
    }
    if (PICK_BUTTON..PICK_BUTTON + 5).contains(&id) {
        return Some(Signal::Pick(id - PICK_BUTTON));
    }
    if (SWATCH_BUTTON..SWATCH_BUTTON + 5).contains(&id) {
        return Some(Signal::Pick(id - SWATCH_BUTTON));
    }
    match id {
        LEGACY => Some(Signal::Legacy),
        RESET => Some(Signal::Reset),
        _ => None,
    }
}
fn color(value: &str) -> COLORREF {
    let value = u32::from_str_radix(value.trim_start_matches('#'), 16).unwrap_or(0);
    ((value >> 16) & 255) | (value & 0xff00) | ((value & 255) << 16)
}
struct Preset {
    id: String,
    name: String,
    button: HWND,
    swatches: Vec<(HWND, String)>,
}
pub(super) struct ThemePanel {
    presets: Vec<Preset>,
    heading: HWND,
    hint: HWND,
    legacy: HWND,
    reset: HWND,
    pickers: [HWND; 5],
    swatches: [HWND; 5],
    effective: [String; 5],
    custom: [COLORREF; 16],
    picker_status: Option<String>,
    pub(super) reset_snapshot: Option<[String; 5]>,
}
impl ThemePanel {
    pub(super) fn new(panel: &Panel) -> anyhow::Result<Self> {
        let mut presets = Vec::new();
        for (index, preset) in crate::theme::presets().iter().enumerate() {
            let resolved = crate::theme::resolve_preset(preset.id)?;
            let button = panel.child_in(
                panel.viewport,
                "BUTTON",
                preset.name,
                PRESET_BUTTON + index,
                WS_TABSTOP | BS_NOTIFY as u32,
            )?;
            let colors = [resolved.background, resolved.foreground]
                .into_iter()
                .chain(resolved.palette.into_iter().skip(1).take(6));
            let mut swatches = Vec::new();
            for (swatch_index, value) in colors.enumerate() {
                let hwnd = panel.child_in(
                    panel.viewport,
                    "BUTTON",
                    &value,
                    7400 + index * 8 + swatch_index,
                    0,
                )?;
                chrome::register_swatch(hwnd, color(&value));
                swatches.push((hwnd, value));
            }
            presets.push(Preset {
                id: preset.id.into(),
                name: preset.name.into(),
                button,
                swatches,
            });
        }
        let mut pickers = [std::ptr::null_mut(); 5];
        let mut swatches = [std::ptr::null_mut(); 5];
        for index in 0..5 {
            pickers[index] = panel.child_in(
                panel.viewport,
                "BUTTON",
                "Choose…",
                PICK_BUTTON + index,
                WS_TABSTOP,
            )?;
            swatches[index] =
                panel.child_in(panel.viewport, "BUTTON", "", SWATCH_BUTTON + index, 0)?;
            chrome::register_swatch(swatches[index], 0);
        }
        Ok(Self {
            presets,
            heading: panel.child_in(
                panel.viewport,
                "STATIC",
                "Custom colors",
                7302,
                SS_NOPREFIX,
            )?,
            hint: panel.child_in(
                panel.viewport,
                "STATIC",
                "Use #RRGGBB; leave blank to inherit. Swatches show the saved effective colors.",
                7303,
                SS_NOPREFIX,
            )?,
            legacy: panel.child_in(
                panel.viewport,
                "BUTTON",
                "Use legacy dark/light",
                LEGACY,
                WS_TABSTOP,
            )?,
            reset: panel.child_in(
                panel.viewport,
                "BUTTON",
                "Reset custom colors",
                RESET,
                WS_TABSTOP,
            )?,
            pickers,
            swatches,
            effective: std::array::from_fn(|_| String::new()),
            custom: [0; 16],
            picker_status: None,
            reset_snapshot: None,
        })
    }
    pub(super) fn preset(&self, index: usize) -> Option<&str> {
        self.presets.get(index).map(|p| p.id.as_str())
    }
    pub(super) fn sync(&mut self, settings: &crate::settings::TerminalSettings) {
        let resolved = crate::theme::resolve(settings);
        self.effective = [
            resolved.background.clone(),
            resolved.foreground.clone(),
            resolved.cursor,
            resolved.selection_background.unwrap_or(resolved.background),
            resolved.selection_foreground.unwrap_or(resolved.foreground),
        ];
        for (index, value) in self.effective.iter().enumerate() {
            chrome::set_swatch(self.swatches[index], color(value));
            unsafe {
                SetWindowTextW(self.swatches[index], wide(value).as_ptr());
            }
        }
        for preset in &self.presets {
            chrome::set_role(
                preset.button,
                chrome::Role::Choice {
                    selected: settings.theme_preset.as_deref() == Some(preset.id.as_str()),
                },
            );
        }
        chrome::set_role(
            self.legacy,
            chrome::Role::Choice {
                selected: settings.theme_preset.is_none(),
            },
        );
    }
    pub(super) fn layout(&self, panel: &Panel, visible: bool, width: i32, dpi: u32, offset: i32) {
        let px = |n: i32| n * dpi.max(96) as i32 / 96;
        unsafe {
            let place = |hwnd, x, y, w: i32, h: i32| {
                ShowWindow(hwnd, if visible { SW_SHOWNA } else { SW_HIDE });
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    x,
                    y - offset,
                    w.max(1),
                    h.max(1),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            };
            place(self.legacy, width - px(178), px(44), px(170), px(30));
            for (index, preset) in self.presets.iter().enumerate() {
                let y = 96 + index as i32 * 40;
                place(preset.button, px(8), px(y), width - px(208), px(34));
                for (index, (hwnd, _)) in preset.swatches.iter().enumerate() {
                    place(
                        *hwnd,
                        width - px(192) + px(index as i32 * 22),
                        px(y + 9),
                        px(16),
                        px(16),
                    );
                }
            }
            place(self.heading, px(8), px(544), width - px(16), px(24));
            place(self.hint, px(8), px(570), width - px(16), px(36));
            for index in 0..5 {
                let y = 612 + index as i32 * 46;
                let row = &panel.rows[FIRST_COLOR + index];
                place(row.label, px(8), px(y), px(200), px(30));
                place(row.input, px(214), px(y), width - px(354), px(30));
                place(self.swatches[index], width - px(128), px(y), px(28), px(28));
                place(self.pickers[index], width - px(92), px(y), px(84), px(30));
            }
            place(self.reset, px(8), px(852), px(184), px(30));
        }
    }
    pub(super) fn enable(&self, idle: bool) {
        unsafe {
            for hwnd in self
                .presets
                .iter()
                .map(|p| p.button)
                .chain([self.legacy, self.reset])
                .chain(self.pickers)
                .chain(self.swatches)
            {
                EnableWindow(hwnd, i32::from(idle));
            }
        }
    }
    pub(super) fn reveal_delta(&self, viewport: HWND, index: usize) -> i32 {
        let Some(preset) = self.presets.get(index) else {
            return 0;
        };
        unsafe {
            let mut row = RECT::default();
            let mut view = RECT::default();
            GetWindowRect(preset.button, &mut row);
            GetWindowRect(viewport, &mut view);
            if row.top < view.top {
                row.top - view.top - 8
            } else if row.bottom > view.bottom {
                row.bottom - view.bottom + 8
            } else {
                0
            }
        }
    }
    pub(super) fn choose(
        &mut self,
        owner: HWND,
        index: usize,
        background: bool,
    ) -> anyhow::Result<Option<String>> {
        anyhow::ensure!(index < 5, "unknown theme color");
        if background {
            self.picker_status = Some("The native color dialog is disabled during hidden verification; use the hex field.".into());
            return Ok(None);
        }
        self.picker_status = None;
        Ok(chrome::choose_color(
            owner,
            color(&self.effective[index]),
            &mut self.custom,
            background,
        )?
        .map(|value| {
            format!(
                "#{:02x}{:02x}{:02x}",
                value & 255,
                (value >> 8) & 255,
                (value >> 16) & 255
            )
        }))
    }
    pub(super) fn diagnostics(&self, panel: &Panel) -> Value {
        json!({"legacy":self.legacy as usize,"reset":self.reset as usize,"picker_available":!panel.background,"picker_status":self.picker_status,
            "preset_input":panel.rows[PRESET].input as usize,"overrides_input":panel.rows[OVERRIDES].input as usize,
            "presets":self.presets.iter().map(|p|json!({"id":p.id,"name":p.name,"button":p.button as usize,"selected":Panel::value(&panel.rows[PRESET])==p.id,"swatches":p.swatches.iter().map(|(hwnd,color)|json!({"handle":*hwnd as usize,"color":color})).collect::<Vec<_>>()})).collect::<Vec<_>>(),
            "fields":(0..5).map(|index|{let row=&panel.rows[FIRST_COLOR+index];json!({"key":row.key,"input":row.input as usize,"picker":self.pickers[index] as usize,"swatch":self.swatches[index] as usize,"effective":self.effective[index],"raw":Panel::value(row),"baseline":row.baseline,"error":row.error})}).collect::<Vec<_>>()})
    }
}
