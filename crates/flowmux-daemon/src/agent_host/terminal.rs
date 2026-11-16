// SPDX-License-Identifier: GPL-3.0-or-later
//! The Host parses provider output once. Views receive a rendering, never
//! provider queries or replayable clipboard/notification escape sequences.

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, Osc52, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Processor, Rgb};
use alacritty_terminal::Term;
use flowmux_ipc::agent_host::TerminalSize;
use flowmux_notify::stream::MAX_OSC_PAYLOAD;
use std::fmt::Write;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const HISTORY_CELLS: usize = 100_000;
const MAX_COMBINING: usize = 32;

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }
    fn screen_lines(&self) -> usize {
        usize::from(self.0.rows)
    }
    fn columns(&self) -> usize {
        usize::from(self.0.cols)
    }
}

struct Size(TerminalSize);

#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<Event>>>);

impl EventListener for Events {
    fn send_event(&self, event: Event) {
        // Queries are bounded by one PTY read; discard non-transport effects.
        if matches!(
            event,
            Event::PtyWrite(_) | Event::ColorRequest(..) | Event::TextAreaSizeRequest(_)
        ) {
            self.0.lock().unwrap().push(event);
        }
    }
}

#[derive(PartialEq, Eq)]
struct Projection {
    size: TerminalSize,
    alternate: bool,
    history: Vec<String>,
    rows: Vec<String>,
    trailer: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OscState {
    Ground,
    Escape,
    Payload,
    Overflow,
}

pub(super) struct Terminal {
    term: Term<Events>,
    // shortcut: synchronized batches bypass the combining bound; cap them before release.
    parser: Processor,
    osc_state: OscState,
    osc: Vec<u8>,
    events: Events,
    size: TerminalSize,
    dirty: bool,
    sequence: u64,
    previous: Option<Projection>,
    diff: Arc<str>,
    snapshot: Option<Arc<str>>,
}

impl Terminal {
    pub fn new(size: TerminalSize) -> Self {
        let events = Events::default();
        let config = Config {
            scrolling_history: HISTORY_CELLS / usize::from(size.cols),
            // Clipboard changes belong to a live, consenting view.
            osc52: Osc52::Disabled,
            ..Config::default()
        };
        Self {
            term: Term::new(config, &Size(size), events.clone()),
            parser: Processor::new(),
            osc_state: OscState::Ground,
            osc: Vec::new(),
            events,
            size,
            dirty: true,
            sequence: 0,
            previous: None,
            diff: Arc::from(""),
            snapshot: None,
        }
    }

    pub fn feed(&mut self, data: &[u8]) -> Vec<u8> {
        // The upstream std parser grows OSC storage without a limit. Admit a
        // complete bounded payload, or cancel before it enters that parser.
        let mut start = 0;
        for (index, &byte) in data.iter().enumerate() {
            match self.osc_state {
                OscState::Ground => {
                    if byte == 0x1b {
                        self.osc_state = OscState::Escape;
                    }
                }
                OscState::Escape => {
                    if byte == b']' {
                        self.advance(&data[start..index]);
                        start = index + 1;
                        self.osc_state = OscState::Payload;
                    } else if matches!(byte, 0x18 | 0x1a | 0x20..=0x7e) {
                        self.osc_state = OscState::Ground;
                    }
                }
                OscState::Payload => {
                    if matches!(byte, 0x07 | 0x18 | 0x1a | 0x1b) {
                        let mut payload = std::mem::take(&mut self.osc);
                        self.advance(b"]");
                        self.advance(&payload);
                        payload.clear();
                        self.osc = payload;
                        self.advance(&data[index..=index]);
                        self.osc_state = if byte == 0x1b {
                            OscState::Escape
                        } else {
                            OscState::Ground
                        };
                    } else if self.osc.len() == MAX_OSC_PAYLOAD {
                        self.osc.clear();
                        self.advance(b"\x18");
                        self.osc_state = OscState::Overflow;
                    } else {
                        self.osc.push(byte);
                    }
                    start = index + 1;
                }
                OscState::Overflow => {
                    if byte == 0x1b {
                        self.advance(&data[index..=index]);
                        self.osc_state = OscState::Escape;
                    } else if matches!(byte, 0x07 | 0x18 | 0x1a) {
                        self.osc_state = OscState::Ground;
                    }
                    start = index + 1;
                }
            }
        }
        self.advance(&data[start..]);
        self.dirty = true;
        self.take_replies()
    }

    fn advance(&mut self, data: &[u8]) {
        let mut start = 0;
        for index in data
            .iter()
            .enumerate()
            .filter_map(|(index, byte)| (*byte >= 128).then_some(index))
        {
            self.parser.advance(&mut self.term, &data[start..=index]);
            self.bound_combining();
            start = index + 1;
        }
        self.parser.advance(&mut self.term, &data[start..]);
    }

    pub fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    pub fn flush_sync(&mut self) -> Vec<u8> {
        self.parser.stop_sync(&mut self.term);
        self.dirty = true;
        self.take_replies()
    }

    fn take_replies(&self) -> Vec<u8> {
        let mut replies = Vec::new();
        for event in self.events.0.lock().unwrap().drain(..) {
            let reply = match event {
                Event::PtyWrite(reply) => reply,
                Event::ColorRequest(index, format) => {
                    format(self.term.colors()[index].unwrap_or_else(|| default_color(index)))
                }
                Event::TextAreaSizeRequest(format) => format(WindowSize {
                    num_lines: self.size.rows,
                    num_cols: self.size.cols,
                    cell_width: 0,
                    cell_height: 0,
                }),
                _ => continue,
            };
            replies.extend_from_slice(reply.as_bytes());
        }
        replies
    }

    fn bound_combining(&mut self) {
        // Bound marks before a later cursor move/alternate-screen switch can
        // hide the cell. ASCII output keeps the parser's bulk fast path.
        let point = self.term.grid().cursor.point;
        let preceding = (point.column.0 == 0 && point.line.0 > 0)
            .then_some((point.line.0 - 1, usize::from(self.size.cols) - 1));
        let points = (point.column.0.saturating_sub(2)..=point.column.0)
            .map(|column| (point.line.0, column))
            .chain(preceding);
        for (line, column) in points {
            let cell = &mut self.term.grid_mut()[Line(line)][Column(column)];
            if let Some(extra) = cell.zerowidth().filter(|extra| extra.len() > MAX_COMBINING) {
                let marks: [char; MAX_COMBINING] = std::array::from_fn(|index| extra[index]);
                let underline = cell.underline_color();
                let link = cell.hyperlink();
                cell.extra = None;
                for mark in marks {
                    cell.push_zerowidth(mark);
                }
                cell.set_underline_color(underline);
                cell.set_hyperlink(link);
            }
        }
    }

    pub fn resize(&mut self, size: TerminalSize) {
        if self.size != size {
            self.term.resize(Size(size));
            let config = Config {
                scrolling_history: HISTORY_CELLS / usize::from(size.cols),
                osc52: Osc52::Disabled,
                ..Config::default()
            };
            self.term.set_options(config);
            self.size = size;
            self.dirty = true;
        }
    }

    pub fn frame(&mut self, after: u64) -> (u64, bool, TerminalSize, Arc<str>) {
        if self.dirty {
            let next = self.project();
            if self.previous.as_ref() != Some(&next) {
                self.diff = Arc::from(match self.previous.as_ref() {
                    Some(previous) => next.diff(previous),
                    None => next.snapshot(),
                });
                self.previous = Some(next);
                self.sequence += 1;
                self.snapshot = None;
            }
            self.dirty = false;
        }
        if after == self.sequence {
            return (self.sequence, false, self.size, Arc::from(""));
        }
        if after != 0 && after.checked_add(1) == Some(self.sequence) {
            return (self.sequence, false, self.size, self.diff.clone());
        }
        let snapshot = self
            .snapshot
            .get_or_insert_with(|| Arc::from(self.previous.as_ref().unwrap().snapshot()));
        (self.sequence, true, self.size, snapshot.clone())
    }

    fn project(&self) -> Projection {
        let grid = self.term.grid();
        let row = |line: i32| {
            let mut result = String::new();
            let mut previous: Option<&Cell> = None;
            let mut link = None;
            for column in 0..usize::from(self.size.cols) {
                let cell = &grid[Line(line)][Column(column)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                if previous.is_none_or(|previous| {
                    previous.fg != cell.fg
                        || previous.bg != cell.bg
                        || previous.flags != cell.flags
                        || previous.underline_color() != cell.underline_color()
                }) {
                    style(&mut result, cell);
                }
                let next_link = cell.hyperlink();
                if link != next_link {
                    result.push_str("\x1b]8;;\x1b\\");
                    if let Some(next_link) = next_link.as_ref().filter(|link| safe_osc(link.uri()))
                    {
                        let _ = write!(result, "\x1b]8;;{}\x1b\\", next_link.uri());
                    }
                    link = next_link;
                }
                result.push(if cell.c.is_control() { ' ' } else { cell.c });
                if let Some(marks) = cell.zerowidth() {
                    result.extend(
                        marks
                            .iter()
                            .copied()
                            .take(MAX_COMBINING)
                            .filter(|c| !c.is_control()),
                    );
                }
                previous = Some(cell);
            }
            result.push_str("\x1b]8;;\x1b\\\x1b[0m");
            result
        };
        let history = (-(grid.history_size() as i32)..0).map(row).collect();
        let rows = (0..i32::from(self.size.rows)).map(row).collect();
        let cursor = grid.cursor.point;
        let mut trailer = format!("\x1b[0m\x1b[{};{}H", cursor.line.0 + 1, cursor.column.0 + 1);
        for (flag, mode) in [
            (TermMode::SHOW_CURSOR, 25),
            (TermMode::APP_CURSOR, 1),
            (TermMode::BRACKETED_PASTE, 2004),
            (TermMode::FOCUS_IN_OUT, 1004),
            (TermMode::MOUSE_REPORT_CLICK, 1000),
            (TermMode::MOUSE_DRAG, 1002),
            (TermMode::MOUSE_MOTION, 1003),
            (TermMode::SGR_MOUSE, 1006),
            (TermMode::UTF8_MOUSE, 1005),
        ] {
            let enabled = self.term.mode().contains(flag);
            let _ = write!(trailer, "\x1b[?{mode}{}", if enabled { 'h' } else { 'l' });
        }
        trailer.push_str(if self.term.mode().contains(TermMode::APP_KEYPAD) {
            "\x1b="
        } else {
            "\x1b>"
        });
        let cursor_style = self.term.cursor_style();
        let shape = match cursor_style.shape {
            CursorShape::Block | CursorShape::HollowBlock | CursorShape::Hidden => 1,
            CursorShape::Underline => 3,
            CursorShape::Beam => 5,
        };
        let _ = write!(
            trailer,
            "\x1b[{} q",
            shape + u8::from(!cursor_style.blinking)
        );
        Projection {
            size: self.size,
            alternate: self.term.mode().contains(TermMode::ALT_SCREEN),
            history,
            rows,
            trailer,
        }
    }
}

impl Projection {
    fn snapshot(&self) -> String {
        let mut data = String::from("\x1bc");
        if self.alternate {
            data.push_str("\x1b[?1049h");
        }
        data.push_str("\x1b[?7l\x1b[H");
        self.write_rows(&mut data, &self.history);
        data.push_str(&self.trailer);
        data
    }

    fn write_rows(&self, data: &mut String, history: &[String]) {
        for (index, row) in history.iter().chain(&self.rows).enumerate() {
            if index != 0 {
                data.push_str("\r\n");
            }
            data.push_str(row);
        }
    }

    fn diff(&self, previous: &Self) -> String {
        if self.size != previous.size || self.alternate != previous.alternate {
            return self.snapshot();
        }
        let mut data = String::from("\x1b[?25l\x1b[?7l\x1b[?6l\x1b[r");
        if self.history != previous.history {
            let overlap = history_overlap(&previous.history, &self.history);
            if overlap == 0 && !previous.history.is_empty() {
                return self.snapshot();
            }
            data.push_str("\x1b[H");
            self.write_rows(&mut data, &self.history[overlap..]);
        } else {
            for (index, (row, old)) in self.rows.iter().zip(&previous.rows).enumerate() {
                if row != old {
                    let _ = write!(data, "\x1b[{};1H{row}", index + 1);
                }
            }
        }
        data.push_str(&self.trailer);
        data
    }
}

fn history_overlap(previous: &[String], next: &[String]) -> usize {
    if next.is_empty() {
        return 0;
    }
    // Prefix matching keeps repeated output linear instead of testing every suffix.
    let mut prefix = vec![0; next.len()];
    for index in 1..next.len() {
        let mut matched = prefix[index - 1];
        while matched > 0 && next[index] != next[matched] {
            matched = prefix[matched - 1];
        }
        prefix[index] = matched + usize::from(next[index] == next[matched]);
    }
    let mut matched = 0;
    for row in previous {
        if matched == next.len() {
            matched = prefix[matched - 1];
        }
        while matched > 0 && row != &next[matched] {
            matched = prefix[matched - 1];
        }
        matched += usize::from(row == &next[matched]);
    }
    matched
}

fn safe_osc(value: &str) -> bool {
    value.len() <= 4096 && !value.chars().any(char::is_control)
}

fn style(data: &mut String, cell: &Cell) {
    data.push_str("\x1b[0");
    for (flag, code) in [
        (Flags::BOLD, "1"),
        (Flags::DIM, "2"),
        (Flags::ITALIC, "3"),
        (Flags::INVERSE, "7"),
        (Flags::HIDDEN, "8"),
        (Flags::STRIKEOUT, "9"),
        (Flags::UNDERLINE, "4"),
        (Flags::DOUBLE_UNDERLINE, "4:2"),
        (Flags::UNDERCURL, "4:3"),
        (Flags::DOTTED_UNDERLINE, "4:4"),
        (Flags::DASHED_UNDERLINE, "4:5"),
    ] {
        if cell.flags.contains(flag) {
            let _ = write!(data, ";{code}");
        }
    }
    color(data, cell.fg, 38);
    color(data, cell.bg, 48);
    if let Some(underline) = cell.underline_color() {
        color(data, underline, 58);
    }
    data.push('m');
}

fn color(data: &mut String, color: Color, target: u8) {
    match color {
        Color::Spec(Rgb { r, g, b }) => {
            let _ = write!(data, ";{target};2;{r};{g};{b}");
        }
        Color::Indexed(index) => {
            let _ = write!(data, ";{target};5;{index}");
        }
        Color::Named(name) if (name as usize) < 16 => {
            let _ = write!(data, ";{target};5;{}", name as usize);
        }
        Color::Named(NamedColor::Background) => {
            let _ = write!(data, ";{}", if target == 48 { 49 } else { 39 });
        }
        Color::Named(_) => {
            let _ = write!(data, ";{}", if target == 48 { 49 } else { 39 });
        }
    }
}

fn default_color(index: usize) -> Rgb {
    let (r, g, b) = match index {
        0..=15 => {
            const ANSI: [(u8, u8, u8); 16] = [
                (0, 0, 0),
                (205, 0, 0),
                (0, 205, 0),
                (205, 205, 0),
                (0, 0, 238),
                (205, 0, 205),
                (0, 205, 205),
                (229, 229, 229),
                (127, 127, 127),
                (255, 0, 0),
                (0, 255, 0),
                (255, 255, 0),
                (92, 92, 255),
                (255, 0, 255),
                (0, 255, 255),
                (255, 255, 255),
            ];
            ANSI[index]
        }
        16..=231 => {
            let i = index - 16;
            let c = |i| if i == 0 { 0 } else { 55 + 40 * i as u8 };
            (c(i / 36), c(i / 6 % 6), c(i % 6))
        }
        232..=255 => {
            let c = 8 + 10 * (index - 232) as u8;
            (c, c, c)
        }
        257 => (0, 0, 0),
        _ => (229, 229, 229),
    };
    Rgb { r, g, b }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_overlap_matches_the_longest_retained_suffix_in_repeated_output() {
        for (before, after, expected) in [
            (vec![], vec!["a"], 0),
            (vec!["a"], vec![], 0),
            (vec!["a", "b", "c"], vec!["b", "c", "d"], 2),
            (vec!["a", "b", "a", "b"], vec!["a", "b"], 2),
            (vec!["a", "a", "a", "b"], vec!["a", "a", "a", "c"], 0),
        ] {
            let before: Vec<_> = before.into_iter().map(String::from).collect();
            let after: Vec<_> = after.into_iter().map(String::from).collect();
            assert_eq!(history_overlap(&before, &after), expected);
        }
    }

    fn visible(terminal: &Terminal) -> Vec<String> {
        let grid = terminal.term.grid();
        (0..i32::from(terminal.size.rows))
            .map(|line| {
                (0..usize::from(terminal.size.cols))
                    .filter_map(|column| {
                        let cell = &grid[Line(line)][Column(column)];
                        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                            return None;
                        }
                        let mut text = cell.c.to_string();
                        text.extend(cell.zerowidth().unwrap_or_default());
                        Some(text)
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn rendered_projection_preserves_wide_combining_cursor_modes_and_reflow() {
        let size = TerminalSize { rows: 5, cols: 12 };
        let mut source = Terminal::new(size);
        source.feed("앞쪽 한글 e\u{301}\r\n두번째 줄\x1b[?2004h\x1b[?1006h\x1b[3;4H".as_bytes());
        let (sequence, snapshot, _, data) = source.frame(0);
        assert!(snapshot);
        let mut view = Terminal::new(size);
        view.feed(data.as_bytes());
        assert_eq!(visible(&source), visible(&view));
        assert_eq!(
            source.term.grid().cursor.point,
            view.term.grid().cursor.point
        );
        assert!(view
            .term
            .mode()
            .contains(TermMode::BRACKETED_PASTE | TermMode::SGR_MOUSE));
        source.resize(TerminalSize { rows: 5, cols: 8 });
        view.resize(TerminalSize { rows: 5, cols: 8 });
        let (_, _, _, data) = source.frame(sequence);
        view.feed(data.as_bytes());
        assert_eq!(visible(&source), visible(&view));
    }

    #[test]
    fn combining_storage_is_bounded_before_cursor_and_buffer_changes_without_losing_style() {
        let size = TerminalSize { rows: 3, cols: 12 };
        let mut source = Terminal::new(size);
        let output = format!(
            "\x1b[1;3;4;38;2;1;2;3;58;2;4;5;6me{}\x1b[2;1H\x1b[?1049h",
            "\u{301}".repeat(10_000)
        );
        source.feed(output.as_bytes());
        source.feed(b"alternate\x1b[?1049l");
        let cell = source.term.grid()[Line(0)][Column(0)].clone();
        assert_eq!(cell.zerowidth().unwrap().len(), MAX_COMBINING);
        let (_, _, _, data) = source.frame(0);
        let mut view = Terminal::new(size);
        view.feed(data.as_bytes());
        let rendered = &view.term.grid()[Line(0)][Column(0)];
        assert_eq!(cell.zerowidth(), rendered.zerowidth());
        assert_eq!(cell.fg, rendered.fg);
        assert_eq!(cell.underline_color(), rendered.underline_color());
        assert!(rendered
            .flags
            .contains(Flags::BOLD | Flags::ITALIC | Flags::UNDERLINE));
        assert_eq!(visible(&source), visible(&view));
    }

    #[test]
    fn wide_character_wrapping_preserves_the_blank_at_the_right_edge() {
        let size = TerminalSize { rows: 3, cols: 12 };
        let mut source = Terminal::new(size);
        source.feed("abcdefghijk한글".as_bytes());
        let (_, _, _, data) = source.frame(0);
        let mut view = Terminal::new(size);
        view.feed(data.as_bytes());
        assert_eq!(visible(&source), visible(&view));
        assert_eq!(
            source.term.grid().cursor.point,
            view.term.grid().cursor.point
        );
    }

    #[test]
    fn queries_are_answered_once_and_side_effects_are_not_replayed() {
        let mut source = Terminal::new(TerminalSize::default());
        let replies =
            source.feed(b"abc\x1b[6n\x1b]52;c;c2VjcmV0\x07\x1b]9;notice\x07\x1b]2;title\x07");
        assert_eq!(replies, b"\x1b[1;4R");
        let (_, _, _, data) = source.frame(0);
        for forbidden in [
            "secret", "c2VjcmV0", "notice", "title", "\x1b[6n", "\x1b]52",
        ] {
            assert!(!data.contains(forbidden));
        }
        let mut view = Terminal::new(TerminalSize::default());
        assert!(view.feed(data.as_bytes()).is_empty());
    }

    #[test]
    fn osc_storage_is_bounded_across_reads_and_recovers_without_partial_hyperlinks() {
        for ending in [b"\x07".as_slice(), b"\x1b\\", b"\x18", b"\x1a"] {
            let mut source = Terminal::new(TerminalSize::default());
            source.feed(b"\x1b]8;;https://oversized.example/");
            for _ in 0..256 {
                source.feed(&[b'x'; 1024]);
                assert!(source.osc.len() <= MAX_OSC_PAYLOAD);
                assert!(source.osc.capacity() <= MAX_OSC_PAYLOAD);
            }
            assert_eq!(source.osc_state, OscState::Overflow);
            assert!(source.osc.is_empty());
            assert!(visible(&source)[0].trim().is_empty());
            source.feed(ending);
            let mut replies = Vec::new();
            for byte in b"plain\x1b]8;id=one;https://example.com\x1b\\linked\x1b]8;;\x07\x1b[6n" {
                replies.extend(source.feed(&[*byte]));
            }
            assert_eq!(replies, b"\x1b[1;12R");
            assert!(visible(&source)[0].starts_with("plainlinked"));
            let grid = source.term.grid();
            assert!(grid[Line(0)][Column(0)].hyperlink().is_none());
            assert_eq!(
                grid[Line(0)][Column(5)].hyperlink().unwrap().uri(),
                "https://example.com"
            );
        }
    }

    #[test]
    fn ignored_escape_bytes_cannot_bypass_the_osc_storage_limit() {
        for byte in [0x00, 0x07, 0x19, 0x1b, 0x1f, 0x7f, 0x80, 0xff] {
            let mut source = Terminal::new(TerminalSize::default());
            source.feed(&[0x1b, byte, b']']);
            source.feed(&vec![b'x'; MAX_OSC_PAYLOAD + 1]);
            assert_eq!(source.osc_state, OscState::Overflow, "{byte:x}");
            assert!(source.osc.is_empty());
            source.feed(b"\x07recovered");
            assert!(visible(&source)[0].starts_with("recovered"));
        }
    }

    #[test]
    fn bounded_osc_preserves_queries_nested_escapes_and_synchronized_output() {
        let mut source = Terminal::new(TerminalSize::default());
        let mut replies = Vec::new();
        for byte in b"\x1b]2;unfinished\x1b]4;1;?\x07\x1b[?2026h\x1b]8;;https://example.com\x1b\\linked\x1b[?2026l" {
            replies.extend(source.feed(&[*byte]));
        }
        assert_eq!(replies, b"\x1b]4;1;rgb:cdcd/0000/0000\x07");
        assert!(source.sync_deadline().is_none());
        assert!(visible(&source)[0].starts_with("linked"));
        assert_eq!(
            source.term.grid()[Line(0)][Column(0)]
                .hyperlink()
                .unwrap()
                .uri(),
            "https://example.com"
        );
    }

    #[test]
    fn synchronized_output_flushes_pending_queries_once_and_resumes_normal_updates() {
        let size = TerminalSize { rows: 3, cols: 12 };
        let mut source = Terminal::new(size);
        source.feed(b"before");
        let (sequence, _, _, _) = source.frame(0);
        assert!(source
            .feed("\x1b[?2026h\r\n한글 e\u{301}\x1b[6n".as_bytes())
            .is_empty());
        assert!(source.sync_deadline().is_some());
        assert_eq!(source.frame(sequence).0, sequence);
        assert_eq!(source.flush_sync(), b"\x1b[2;7R");
        assert!(source.sync_deadline().is_none());
        let (next, _, _, _) = source.frame(sequence);
        assert!(next > sequence);
        assert!(visible(&source)[1].contains("한글 e\u{301}"));
        let mut view = Terminal::new(size);
        view.feed(source.frame(0).3.as_bytes());
        assert_eq!(visible(&source), visible(&view));
        assert!(source.flush_sync().is_empty());
        assert_eq!(source.frame(next).0, next);
        assert!(source.feed(b"\x1b[?2026l\r\nnext").is_empty());
        assert!(source.frame(next).3.contains("next"));

        source.feed(b"\x1b[?2026h\x1b[Hcompleted\x1b[?2026l");
        assert!(source.sync_deadline().is_none());
        assert!(source.frame(0).3.contains("completed"));
    }

    #[test]
    fn diff_and_snapshot_restore_history_and_primary_after_alternate_screen() {
        let size = TerminalSize { rows: 3, cols: 12 };
        let mut source = Terminal::new(size);
        let mut view = Terminal::new(size);
        let mut sequence = 0;
        for output in [
            "first\r\nsecond\r\nthird",
            "\r\nfourth",
            "\r\nfifth",
            "\x1b[?1049h\x1b[2J\x1b[Halt 한글",
            "\x1b[?1049l",
        ] {
            source.feed(output.as_bytes());
            let (next, _, _, data) = source.frame(sequence);
            view.feed(data.as_bytes());
            sequence = next;
            assert_eq!(visible(&source), visible(&view), "{output:?}");
            assert_eq!(
                source.term.grid().history_size(),
                view.term.grid().history_size()
            );
        }
        let (_, _, _, snapshot) = source.frame(0);
        let mut reattached = Terminal::new(size);
        reattached.feed(snapshot.as_bytes());
        assert_eq!(visible(&source), visible(&reattached));
        assert_eq!(
            source.term.grid().history_size(),
            reattached.term.grid().history_size()
        );
    }
}
