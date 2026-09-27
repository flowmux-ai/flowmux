// SPDX-License-Identifier: GPL-3.0-or-later
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, clap::Subcommand, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// Read minimap geometry and a checksum of its current canvas raster.
    Read {},
    /// Move only the preview window; negative rows show older history.
    Preview {
        #[arg(allow_hyphen_values = true)]
        rows: i32,
    },
    /// Center the terminal viewport on a zero-based retained buffer row.
    Seek { row: u32 },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColorCount {
    color: String,
    count: u32,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    enabled: bool,
    visible: bool,
    width: u16,
    opacity: u8,
    buffer: crate::screen::Buffer,
    cols: u16,
    rows: u16,
    buffer_rows: u32,
    viewport_row: u32,
    preview_top: u32,
    preview_rows: u16,
    preview_offset: u32,
    renders: u64,
    gutter: f64,
    raster_width: u16,
    raster_height: u16,
    raster_hash: Option<String>,
    cells: u32,
    ink_cells: u32,
    wide_cells: u32,
    colors: Vec<ColorCount>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Ok { snapshot: Snapshot },
    Error { message: String },
}
impl Outcome {
    pub fn validate(&self) -> anyhow::Result<()> {
        match self {
            Self::Error { message } => {
                anyhow::ensure!(message.len() <= 512, "invalid minimap error")
            }
            Self::Ok { snapshot: s } => {
                anyhow::ensure!(
                    (12..=96).contains(&s.width)
                        && s.opacity <= 100
                        && (2..=1000).contains(&s.cols)
                        && (1..=1000).contains(&s.rows)
                        && (u32::from(s.rows)..=101_000).contains(&s.buffer_rows)
                        && s.viewport_row <= s.buffer_rows - u32::from(s.rows)
                        && (1..=2048).contains(&s.preview_rows)
                        && u64::from(s.preview_top)
                            + u64::from(s.preview_rows)
                            + u64::from(s.preview_offset)
                            == u64::from(s.buffer_rows)
                        && s.gutter.is_finite()
                        && (-1.0..=97.0).contains(&s.gutter)
                        && (1..=192).contains(&s.raster_width)
                        && (1..=4096).contains(&s.raster_height)
                        && s.cells <= 2_048_000
                        && s.ink_cells <= s.cells
                        && s.wide_cells <= s.ink_cells
                        && s.colors.len() <= 32,
                    "invalid minimap geometry"
                );
                anyhow::ensure!(
                    !s.visible
                        || (s.enabled
                            && s.buffer == crate::screen::Buffer::Normal
                            && s.raster_hash.is_some()),
                    "invalid minimap visibility"
                );
                anyhow::ensure!(
                    s.raster_hash
                        .as_ref()
                        .is_none_or(|h| h.len() == 8 && h.bytes().all(|c| c.is_ascii_hexdigit()))
                        && s.colors.iter().all(|c| c.color.len() == 7
                            && c.color.starts_with('#')
                            && c.color[1..].bytes().all(|b| b.is_ascii_hexdigit())
                            && c.count <= s.cells),
                    "invalid minimap raster metadata"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[test]
    fn minimap_commands_preserve_signed_preview_and_strict_action_payloads() {
        let cli = crate::command::Cli::try_parse_from(["flowmuxctl", "minimap", "preview", "-25"])
            .unwrap();
        assert!(matches!(
            cli.command,
            crate::command::Command::Minimap {
                action: Action::Preview { rows: -25 },
                ..
            }
        ));
        assert!(serde_json::from_str::<Action>(r#"{"kind":"seek","row":-1}"#).is_err());
        assert!(serde_json::from_str::<Action>(r#"{"kind":"read","extra":1}"#).is_err());
    }
    #[test]
    fn minimap_metadata_rejects_unbounded_and_inconsistent_raster_values() {
        let value = serde_json::json!({"status":"ok","snapshot":{
            "enabled":true,"visible":true,"width":40,"opacity":50,"buffer":"normal",
            "cols":80,"rows":24,"buffer_rows":1000,"viewport_row":976,
            "preview_top":400,"preview_rows":600,"preview_offset":0,"renders":1,"gutter":40.0,
            "raster_width":40,"raster_height":600,"raster_hash":"a0123bcd",
            "cells":48000,"ink_cells":12000,"wide_cells":1000,"colors":[{"color":"#123456","count":400}]
        }});
        serde_json::from_value::<Outcome>(value.clone())
            .unwrap()
            .validate()
            .unwrap();
        for (key, bad) in [
            ("preview_top", serde_json::json!(u32::MAX)),
            ("visible", serde_json::json!(true)),
            ("raster_height", serde_json::json!(4097)),
            (
                "colors",
                serde_json::json!([{"color":"badcolor","count":1}]),
            ),
        ] {
            let mut changed = value.clone();
            changed["snapshot"][key] = bad;
            if key == "visible" {
                changed["snapshot"]["enabled"] = false.into();
            }
            assert!(serde_json::from_value::<Outcome>(changed)
                .unwrap()
                .validate()
                .is_err());
        }
    }
}
