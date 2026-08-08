// SPDX-License-Identifier: GPL-3.0-or-later
//! Styled terminal scrollback snapshots.
//!
//! VTE deliberately returns no cell attributes for `Format::Text`; its
//! supported styled export is `Format::Html`. VTE's HTML is a small,
//! deterministic fragment (`<pre>` plus formatting tags), so we retain that
//! lossless snapshot in state and translate it back to SGR when replaying into
//! a new terminal widget.

use flowmux_core::{
    bound_terminal_scrollback, bound_vte_html_scrollback, TerminalScrollback,
    TerminalScrollbackFormat, TERMINAL_SCROLLBACK_MAX_BYTES,
};
use quick_xml::escape::resolve_xml_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

const PRE_OPEN: &str = "<pre>";
const PRE_CLOSE: &str = "</pre>";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum UnderlineStyle {
    #[default]
    None,
    Solid,
    Double,
    Wavy,
    Dotted,
    Dashed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TextStyle {
    foreground: Option<[u8; 3]>,
    background: Option<[u8; 3]>,
    underline_color: Option<[u8; 3]>,
    bold: bool,
    italic: bool,
    underline: UnderlineStyle,
    strikethrough: bool,
    overline: bool,
    blink: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StyledRun {
    style: TextStyle,
    text: String,
}

#[derive(Debug)]
struct ParsedSnapshot {
    runs: Vec<StyledRun>,
}

impl ParsedSnapshot {
    fn plain_text(&self) -> String {
        let capacity = self.runs.iter().map(|run| run.text.len()).sum();
        let mut text = String::with_capacity(capacity);
        for run in &self.runs {
            text.push_str(&run.text);
        }
        text
    }
}

pub(crate) fn normalize_plain_text_snapshot(text: &str) -> String {
    let lines: Vec<_> = text.lines().collect();
    let Some(first) = lines.iter().position(|line| !line.trim().is_empty()) else {
        return String::new();
    };
    let last = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .unwrap_or(first);
    let meaningful = &lines[first..=last];
    if meaningful
        .iter()
        .filter(|line| !line.trim().is_empty())
        .count()
        <= 1
    {
        return String::new();
    }
    meaningful.join("\n")
}

/// Validate, trim, and bound VTE's styled export while retaining complete HTML
/// lines. VTE closes formatting spans before every newline, which makes a
/// line-aligned suffix independently valid markup.
pub(crate) fn snapshot_from_vte_html(html: &str) -> Result<TerminalScrollback, String> {
    let body = html
        .strip_prefix(PRE_OPEN)
        .and_then(|value| value.strip_suffix(PRE_CLOSE))
        .ok_or_else(|| "VTE HTML snapshot did not contain a single <pre> root".to_string())?;

    if html.len() > TERMINAL_SCROLLBACK_MAX_BYTES {
        let bounded_html = bound_vte_html_scrollback(html)
            .ok_or_else(|| "VTE HTML final line exceeded the scrollback budget".to_string())?;
        let tail = bounded_html
            .strip_prefix(PRE_OPEN)
            .and_then(|value| value.strip_suffix(PRE_CLOSE))
            .expect("bounded VTE HTML retains its pre root");
        let snapshot = snapshot_from_complete_vte_html(&bounded_html, tail)?;
        return (!snapshot.content().is_empty())
            .then_some(snapshot)
            .ok_or_else(|| "bounded VTE HTML contained too little meaningful history".to_string());
    }

    snapshot_from_complete_vte_html(html, body)
}

fn snapshot_from_complete_vte_html(html: &str, body: &str) -> Result<TerminalScrollback, String> {
    let parsed = parse_vte_html(html)?;
    let plain = parsed.plain_text();
    let html_lines: Vec<_> = body.split('\n').collect();
    let plain_lines: Vec<_> = plain.split('\n').collect();
    if html_lines.len() != plain_lines.len() {
        return Err("VTE HTML line structure did not match its text content".into());
    }

    let Some(first) = plain_lines.iter().position(|line| !line.trim().is_empty()) else {
        return Ok(TerminalScrollback::vte_html(""));
    };
    let last = plain_lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .unwrap_or(first);
    if plain_lines[first..=last]
        .iter()
        .filter(|line| !line.trim().is_empty())
        .count()
        <= 1
    {
        return Ok(TerminalScrollback::vte_html(""));
    }

    let normalized_html = format!(
        "{PRE_OPEN}{}{PRE_CLOSE}",
        html_lines[first..=last].join("\n")
    );
    let snapshot = TerminalScrollback::vte_html(normalized_html);
    if let Some(snapshot) = snapshot.into_bounded() {
        return Ok(snapshot);
    }

    // A single pathological line can exceed the whole HTML budget, leaving no
    // valid tag-aligned suffix. Preserve its text rather than clearing the
    // previous snapshot; only that oversized snapshot loses styling.
    let plain = plain_lines[first..=last].join("\n");
    Ok(TerminalScrollback::plain_text(bound_terminal_scrollback(
        &plain,
    )))
}

pub(crate) fn replay_bytes(snapshot: &TerminalScrollback) -> Result<Option<Vec<u8>>, String> {
    let replay = match snapshot.format() {
        TerminalScrollbackFormat::PlainText => {
            let text = normalize_plain_text_snapshot(snapshot.content());
            if text.is_empty() {
                return Ok(None);
            }
            text
        }
        TerminalScrollbackFormat::VteHtml => {
            if snapshot.content().is_empty() {
                return Ok(None);
            }
            render_ansi(&parse_vte_html(snapshot.content())?.runs)
        }
    };
    Ok((!replay.is_empty()).then(|| with_terminal_line_endings(&replay)))
}

fn parse_vte_html(html: &str) -> Result<ParsedSnapshot, String> {
    let mut reader = Reader::from_str(html);
    reader.config_mut().trim_text(false);
    let mut styles = vec![TextStyle::default()];
    let mut runs = Vec::new();
    let mut saw_pre = false;
    let mut inside_pre = false;

    loop {
        match reader.read_event().map_err(|error| error.to_string())? {
            Event::Start(start) => {
                let name = start.name();
                if name.as_ref() == b"pre" {
                    if saw_pre || inside_pre {
                        return Err("VTE HTML contained multiple <pre> roots".into());
                    }
                    saw_pre = true;
                    inside_pre = true;
                } else if !inside_pre {
                    return Err("VTE HTML contained markup outside <pre>".into());
                }
                let parent = *styles.last().expect("style stack is never empty");
                styles.push(style_for_start(parent, &start)?);
            }
            Event::End(end) => {
                if styles.len() <= 1 {
                    return Err("VTE HTML closed more tags than it opened".into());
                }
                styles.pop();
                if end.name().as_ref() == b"pre" {
                    inside_pre = false;
                }
            }
            Event::Text(text) => {
                let text = text.decode().map_err(|error| error.to_string())?;
                if inside_pre {
                    push_run(
                        &mut runs,
                        *styles.last().expect("style stack is never empty"),
                        text.as_ref(),
                    );
                } else if !text.trim().is_empty() {
                    return Err("VTE HTML contained text outside <pre>".into());
                }
            }
            Event::GeneralRef(reference) => {
                if !inside_pre {
                    return Err("VTE HTML contained an entity outside <pre>".into());
                }
                let value = if let Some(ch) = reference
                    .resolve_char_ref()
                    .map_err(|error| error.to_string())?
                {
                    ch.to_string()
                } else {
                    let name = reference.decode().map_err(|error| error.to_string())?;
                    resolve_xml_entity(name.as_ref())
                        .ok_or_else(|| format!("unsupported VTE HTML entity: &{name};"))?
                        .to_string()
                };
                push_run(
                    &mut runs,
                    *styles.last().expect("style stack is never empty"),
                    &value,
                );
            }
            Event::CData(text) => {
                if !inside_pre {
                    return Err("VTE HTML contained CDATA outside <pre>".into());
                }
                let text = text.decode().map_err(|error| error.to_string())?;
                push_run(
                    &mut runs,
                    *styles.last().expect("style stack is never empty"),
                    text.as_ref(),
                );
            }
            Event::Eof => break,
            Event::Empty(_) | Event::Comment(_) => {}
            Event::Decl(_) | Event::PI(_) | Event::DocType(_) => {
                return Err("VTE HTML contained an unsupported document construct".into());
            }
        }
    }

    if !saw_pre || inside_pre || styles.len() != 1 {
        return Err("VTE HTML had an incomplete <pre> root".into());
    }
    Ok(ParsedSnapshot { runs })
}

fn style_for_start(parent: TextStyle, start: &BytesStart<'_>) -> Result<TextStyle, String> {
    let mut style = parent;
    match start.name().as_ref() {
        b"b" => style.bold = true,
        b"i" => style.italic = true,
        b"u" => {
            style.underline = UnderlineStyle::Solid;
            if let Some(css) = attribute_value(start, b"style")? {
                apply_css(&mut style, &css);
            }
        }
        b"font" => {
            if let Some(color) = attribute_value(start, b"color")? {
                style.foreground = parse_hex_color(&color);
            }
        }
        b"span" => {
            if let Some(css) = attribute_value(start, b"style")? {
                apply_css(&mut style, &css);
            }
        }
        b"strike" => style.strikethrough = true,
        b"blink" => style.blink = true,
        b"pre" => {}
        _ => {}
    }
    Ok(style)
}

fn attribute_value(start: &BytesStart<'_>, key: &[u8]) -> Result<Option<String>, String> {
    for attribute in start.attributes().with_checks(false) {
        let attribute = attribute.map_err(|error| error.to_string())?;
        if attribute.key.as_ref() == key {
            return attribute
                .unescape_value()
                .map(|value| Some(value.into_owned()))
                .map_err(|error| error.to_string());
        }
    }
    Ok(None)
}

fn apply_css(style: &mut TextStyle, css: &str) {
    for declaration in css.split(';') {
        let Some((name, value)) = declaration.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        match name {
            "background-color" => style.background = parse_hex_color(value),
            "text-decoration-color" => style.underline_color = parse_hex_color(value),
            "text-decoration-line" if value == "overline" => style.overline = true,
            "text-decoration-style" => {
                style.underline = match value {
                    "double" => UnderlineStyle::Double,
                    "wavy" => UnderlineStyle::Wavy,
                    "dotted" => UnderlineStyle::Dotted,
                    "dashed" => UnderlineStyle::Dashed,
                    _ => UnderlineStyle::Solid,
                };
            }
            _ => {}
        }
    }
}

fn parse_hex_color(value: &str) -> Option<[u8; 3]> {
    let hex = value.trim().strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    Some([
        u8::from_str_radix(&hex[0..2], 16).ok()?,
        u8::from_str_radix(&hex[2..4], 16).ok()?,
        u8::from_str_radix(&hex[4..6], 16).ok()?,
    ])
}

fn push_run(runs: &mut Vec<StyledRun>, style: TextStyle, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = runs.last_mut().filter(|run| run.style == style) {
        last.text.push_str(text);
    } else {
        runs.push(StyledRun {
            style,
            text: text.to_string(),
        });
    }
}

fn render_ansi(runs: &[StyledRun]) -> String {
    let text_capacity: usize = runs.iter().map(|run| run.text.len()).sum();
    let mut output = String::with_capacity(text_capacity + runs.len() * 16);
    let mut active = TextStyle::default();
    for run in runs {
        if run.text.is_empty() {
            continue;
        }
        if run.style != active {
            output.push_str("\x1b[0m");
            push_style_sgr(&mut output, run.style);
            active = run.style;
        }
        output.push_str(&run.text);
    }
    if active != TextStyle::default() {
        output.push_str("\x1b[0m");
    }
    output
}

fn push_style_sgr(output: &mut String, style: TextStyle) {
    let mut params = Vec::new();
    if style.bold {
        params.push("1".to_string());
    }
    if style.italic {
        params.push("3".to_string());
    }
    match style.underline {
        UnderlineStyle::None => {}
        UnderlineStyle::Solid => params.push("4".into()),
        UnderlineStyle::Double => params.push("4:2".into()),
        UnderlineStyle::Wavy => params.push("4:3".into()),
        UnderlineStyle::Dotted => params.push("4:4".into()),
        UnderlineStyle::Dashed => params.push("4:5".into()),
    }
    if style.blink {
        params.push("5".into());
    }
    if style.strikethrough {
        params.push("9".into());
    }
    if style.overline {
        params.push("53".into());
    }
    push_color_params(&mut params, "38", style.foreground);
    push_color_params(&mut params, "48", style.background);
    push_color_params(&mut params, "58", style.underline_color);
    if !params.is_empty() {
        output.push_str("\x1b[");
        output.push_str(&params.join(";"));
        output.push('m');
    }
}

fn push_color_params(params: &mut Vec<String>, prefix: &str, color: Option<[u8; 3]>) {
    let Some([red, green, blue]) = color else {
        return;
    };
    params.extend([
        prefix.to_string(),
        "2".into(),
        red.to_string(),
        green.to_string(),
        blue.to_string(),
    ]);
}

fn with_terminal_line_endings(text: &str) -> Vec<u8> {
    let mut replay = Vec::with_capacity(text.len() + text.lines().count());
    let mut previous = None;
    for byte in text.bytes() {
        if byte == b'\n' && previous != Some(b'\r') {
            replay.push(b'\r');
        }
        replay.push(byte);
        previous = Some(byte);
    }
    if !text.ends_with('\n') {
        replay.extend_from_slice(b"\r\n");
    }
    replay
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::prelude::*;

    #[test]
    fn plain_snapshot_drops_viewport_padding_and_an_idle_prompt() {
        assert_eq!(normalize_plain_text_snapshot("\n\n\n➜  work \n"), "");
        assert_eq!(normalize_plain_text_snapshot("➜  work \n\n\n"), "");
        assert_eq!(
            normalize_plain_text_snapshot("\n\ncommand output\n\n➜  work \n\n"),
            "command output\n\n➜  work "
        );
    }

    #[test]
    fn styled_snapshot_trims_viewport_padding_without_losing_markup() {
        let snapshot = snapshot_from_vte_html(
            "<pre>\n<b><font color=\"#112233\">colored &amp; bold</font></b>\nplain\n\n</pre>",
        )
        .unwrap();
        assert_eq!(snapshot.format(), TerminalScrollbackFormat::VteHtml);
        assert_eq!(
            snapshot.content(),
            "<pre><b><font color=\"#112233\">colored &amp; bold</font></b>\nplain</pre>"
        );
    }

    #[test]
    fn oversized_styled_snapshot_is_bounded_before_parsing() {
        let old_line = format!("<b>{}</b>", "old".repeat(1024));
        let html = format!(
            "<pre>{}\nrecent\nprompt</pre>",
            std::iter::repeat_n(old_line, 512)
                .collect::<Vec<_>>()
                .join("\n")
        );
        let snapshot = snapshot_from_vte_html(&html).unwrap();
        assert!(snapshot.content().len() <= TERMINAL_SCROLLBACK_MAX_BYTES);
        assert!(snapshot.content().ends_with("recent\nprompt</pre>"));
    }

    #[test]
    fn oversized_single_html_line_uses_plain_text_fallback() {
        let html = format!("<pre>{}</pre>", "x".repeat(TERMINAL_SCROLLBACK_MAX_BYTES));
        assert!(snapshot_from_vte_html(&html).is_err());
    }

    #[test]
    fn styled_replay_restores_foreground_background_and_bold() {
        let snapshot = TerminalScrollback::vte_html(
            "<pre><span style=\"background-color:#445566\"><font color=\"#112233\"><b>color</b></font></span>\nplain</pre>",
        );
        let replay = replay_bytes(&snapshot).unwrap().unwrap();
        assert_eq!(
            replay,
            b"\x1b[0m\x1b[1;38;2;17;34;51;48;2;68;85;102mcolor\x1b[0m\r\nplain\r\n"
        );
    }

    #[test]
    fn styled_replay_maps_extended_vte_attributes_and_entities() {
        let snapshot = TerminalScrollback::vte_html(
            "<pre><blink><strike><span style=\"text-decoration-line:overline\"><u style=\"text-decoration-style:wavy;text-decoration-color:#010203\"><i>A&lt;&amp;&#x1F642;</i></u></span></strike></blink>\nplain</pre>",
        );
        let replay = String::from_utf8(replay_bytes(&snapshot).unwrap().unwrap()).unwrap();
        assert!(replay.contains("\x1b[3;4:3;5;9;53;58;2;1;2;3mA<&🙂"));
        assert!(replay.ends_with("\x1b[0m\r\nplain\r\n"));
    }

    #[test]
    fn legacy_plain_text_replay_keeps_existing_prompt_filter() {
        assert!(replay_bytes(&TerminalScrollback::from("\n\nprompt\n"))
            .unwrap()
            .is_none());
        assert_eq!(
            replay_bytes(&TerminalScrollback::from("output\nprompt"))
                .unwrap()
                .unwrap(),
            b"output\r\nprompt\r\n"
        );
    }

    #[test]
    fn malformed_styled_snapshot_is_rejected() {
        assert!(snapshot_from_vte_html("<pre><b>broken</pre>").is_err());
    }

    #[gtk::test]
    async fn vte_styled_export_can_be_replayed_with_display_attributes() {
        let pane = crate::ui::ghostty_pane::GhosttyPane::spawn(
            flowmux_core::PaneId::new(),
            flowmux_core::SurfaceId::new(),
            vec!["/bin/sh".into()],
            None,
            Vec::new(),
            5_000,
            crate::ui::pane_terminal::PaneCallbacks::noop_for_test(),
        );
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&pane.container));
        window.present();
        gtk::glib::timeout_future(std::time::Duration::from_millis(50)).await;
        pane.write_input(
            b"printf '\\033[1;38;2;17;34;51;48;2;68;85;102mFLOWMUX_STYLED\\033[0m\\nplain\\n'\n",
        )
        .unwrap();
        for _ in 0..40 {
            if pane
                .screen_text()
                .as_deref()
                .is_some_and(|text| text.matches("FLOWMUX_STYLED").count() >= 2)
            {
                break;
            }
            gtk::glib::timeout_future(std::time::Duration::from_millis(25)).await;
        }

        let snapshot = pane
            .scrollback_snapshot()
            .expect("VTE should export terminal history");
        assert_eq!(snapshot.format(), TerminalScrollbackFormat::VteHtml);
        assert!(
            !snapshot.content().is_empty(),
            "VTE snapshot unexpectedly empty"
        );

        let replay = String::from_utf8(replay_bytes(&snapshot).unwrap().unwrap()).unwrap();
        assert!(
            replay.contains("\x1b[1;38;2;17;34;51;48;2;68;85;102mFLOWMUX_STYLED"),
            "VTE foreground, background, and bold attributes must survive replay; snapshot: {}; replay: {replay:?}",
            snapshot.content()
        );
        pane.close_pty();
        window.close();
    }
}
