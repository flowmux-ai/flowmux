// SPDX-License-Identifier: GPL-3.0-or-later
//! The Host parses provider output once. Views receive a rendering, never
//! provider queries or replayable clipboard/notification escape sequences.

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Charsets, Dimensions};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, CellExtra, Flags};
use alacritty_terminal::term::{Config, Osc52, TermMode};
use alacritty_terminal::vte::ansi::{
    self, CharsetIndex, Color, CursorShape, Handler, NamedColor, Processor, Rgb, StandardCharset,
};
use alacritty_terminal::Term;
use flowmux_ipc::agent_host::{TerminalColors, TerminalSize};
use flowmux_notify::stream::MAX_OSC_PAYLOAD;
use std::collections::{HashSet, VecDeque};
use std::fmt::Write;
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

const HISTORY_CELLS: usize = 100_000;
/// A shell's tab restores only what the Host kept, so a shell keeps more than an agent's screen
/// needs.
const SHELL_HISTORY_CELLS: usize = 400_000;
/// Output a passthrough view may fall behind by before it is redrawn instead.
const MAX_PASSTHROUGH_BYTES: usize = 1024 * 1024;
/// A shell's first output kept for the view that opens it.
const MAX_INTRODUCTION_BYTES: usize = 64 * 1024;
const MAX_COMBINING: usize = 32;
const MAX_TITLE_BYTES: usize = 4096;
const MAX_PROJECTION_BYTES: usize = 16 * 1024 * 1024;
const MAX_PROJECTION_LINK_BYTES: usize = 256 * 1024;
const MAX_RETAINED_LINK_BYTES: usize = 4 * 1024 * 1024;

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
struct Events {
    replies: Arc<Mutex<Vec<Event>>>,
    title: Arc<Mutex<Option<String>>>,
}

impl EventListener for Events {
    fn send_event(&self, event: Event) {
        match event {
            Event::Title(title) => {
                *self.title.lock().unwrap() = Some(title);
                return;
            }
            Event::ResetTitle => {
                *self.title.lock().unwrap() = None;
                return;
            }
            _ => {}
        }
        // Queries are bounded by one PTY read; discard non-transport effects.
        if matches!(
            event,
            Event::PtyWrite(_) | Event::ColorRequest(..) | Event::TextAreaSizeRequest(_)
        ) {
            self.replies.lock().unwrap().push(event);
        }
    }
}

#[derive(PartialEq, Eq)]
struct Projection {
    size: TerminalSize,
    alternate: bool,
    history: Vec<String>,
    rows: Vec<String>,
    /// For a passthrough view, whether each history and screen row wraps into the next.
    soft: Vec<bool>,
    trailer: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OscState {
    Ground,
    Escape,
    Payload,
    Overflow,
}

/// Where program output stands between escape sequences and characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sequence {
    Ground,
    /// Continuation bytes still expected.
    Character(u8),
    Escape,
    Intermediate,
    Control,
    /// A string that ends at ST, and at BEL when `true`.
    String(bool),
    StringEscape,
}

pub(super) struct Terminal {
    term: Term<Events>,
    links: LinkBudget,
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
    /// The attached view's defaults; only query replies read them, never the projection.
    colors: Option<TerminalColors>,
    /// A shell's view receives the program's own bytes. The emulated screen only redraws a view
    /// that connects or fell behind.
    passthrough: bool,
    /// The view is larger than this terminal, so it cannot follow the program's own bytes and
    /// is drawn from this screen, as an agent's view is.
    rendered: bool,
    /// The passthrough view was last drawn on the alternate screen, so its own terminal
    /// holds no primary screen to return to.
    view_lacks_primary: bool,
    parse: Sequence,
    /// The bytes of the escape sequence or character that output has begun and not ended. A
    /// redrawn view needs them before the rest arrives.
    pending: Vec<u8>,
    /// Bytes not yet framed; `None` until the view has been redrawn.
    unframed: Option<Vec<u8>>,
    /// A new shell's output so far. Its first view gets these bytes, not a redraw that would clear
    /// the tab.
    introduction: Option<Vec<u8>>,
}

// Intercept parser input itself: sync release and REP can emit many characters
// without returning to Terminal::advance, including cells on the inactive grid.
struct BoundedTerm<'a>(&'a mut Term<Events>, &'a mut LinkBudget);

#[derive(Default)]
struct LinkBudget {
    cells: HashSet<usize>,
    allocations: VecDeque<(Weak<CellExtra>, usize)>,
    bytes: usize,
}

impl LinkBudget {
    fn admit(&mut self, cell: &mut Cell) {
        let Some(extra) = &cell.extra else { return };
        let Some(link) = cell.hyperlink() else { return };
        let pointer = Arc::as_ptr(extra) as usize;
        if self.cells.contains(&pointer) {
            return;
        }
        // Charge shared link payloads conservatively, including allocation and tracking overhead.
        let bytes = 2 * (link.uri().len() + link.id().len()) + 256;
        if self.bytes + bytes > MAX_RETAINED_LINK_BYTES {
            // Bound reclamation work too; repeated denied OSCs cannot rescan the whole grid.
            for _ in 0..self.allocations.len().min(32) {
                let (cell, bytes) = self.allocations.pop_front().unwrap();
                if cell.strong_count() == 0 {
                    self.cells.remove(&(cell.as_ptr() as usize));
                    self.bytes -= bytes;
                } else {
                    self.allocations.push_back((cell, bytes));
                }
            }
        }
        if self.bytes + bytes > MAX_RETAINED_LINK_BYTES {
            cell.set_hyperlink(None);
        } else {
            self.cells.insert(pointer);
            self.allocations.push_back((Arc::downgrade(extra), bytes));
            self.bytes += bytes;
        }
    }
}

macro_rules! forward_terminal {
    ($(fn $name:ident($($arg:ident: $ty:ty),*);)*) => {
        $(fn $name(&mut self, $($arg: $ty),*) {
            self.0.$name($($arg),*);
        })*
    };
}

impl Handler for BoundedTerm<'_> {
    fn set_title(&mut self, title: Option<String>) {
        // The engine clones titles into its 4096-entry saved-title stack.
        self.0.set_title(title.map(|mut title| {
            title.truncate(title.floor_char_boundary(MAX_TITLE_BYTES));
            title
        }));
    }

    fn input(&mut self, c: char) {
        let point = self.0.grid().cursor.point;
        let clears_wide =
            point.column.0 > 0 && self.0.grid()[point].flags.contains(Flags::WIDE_CHAR_SPACER);
        self.0.input(c);
        if clears_wide {
            // Overwriting a spacer can clone the preceding cell's link storage.
            self.1
                .admit(&mut self.0.grid_mut()[point.line][point.column - 1]);
        }
        if !c.is_ascii() {
            bound_combining(self.0, self.1);
        }
    }

    fn set_hyperlink(&mut self, hyperlink: Option<ansi::Hyperlink>) {
        // Bound retained links too, including IDs which the projection omits.
        self.0.set_hyperlink(
            hyperlink.filter(|link| safe_osc(&link.uri) && link.id.as_deref().is_none_or(safe_osc)),
        );
        self.1.admit(&mut self.0.grid_mut().cursor.template);
    }

    fn terminal_attribute(&mut self, attr: ansi::Attr) {
        self.0.terminal_attribute(attr);
        self.1.admit(&mut self.0.grid_mut().cursor.template);
    }

    fn reset_state(&mut self) {
        self.0.reset_state();
        self.1.cells.clear();
        self.1.allocations.clear();
        self.1.bytes = 0;
    }

    forward_terminal! {
        fn set_cursor_style(arg1: Option<ansi::CursorStyle>);
        fn set_cursor_shape(arg1: ansi::CursorShape);
        fn goto(arg1: i32, arg2: usize);
        fn goto_line(arg1: i32);
        fn goto_col(arg1: usize);
        fn insert_blank(arg1: usize);
        fn move_up(arg1: usize);
        fn move_down(arg1: usize);
        fn identify_terminal(arg1: Option<char>);
        fn device_status(arg1: usize);
        fn move_forward(arg1: usize);
        fn move_backward(arg1: usize);
        fn move_down_and_cr(arg1: usize);
        fn move_up_and_cr(arg1: usize);
        fn put_tab(arg1: u16);
        fn backspace();
        fn carriage_return();
        fn linefeed();
        fn bell();
        fn substitute();
        fn newline();
        fn set_horizontal_tabstop();
        fn scroll_up(arg1: usize);
        fn scroll_down(arg1: usize);
        fn insert_blank_lines(arg1: usize);
        fn delete_lines(arg1: usize);
        fn erase_chars(arg1: usize);
        fn delete_chars(arg1: usize);
        fn move_backward_tabs(arg1: u16);
        fn move_forward_tabs(arg1: u16);
        fn save_cursor_position();
        fn restore_cursor_position();
        fn clear_line(arg1: ansi::LineClearMode);
        fn clear_screen(arg1: ansi::ClearMode);
        fn clear_tabs(arg1: ansi::TabulationClearMode);
        fn reverse_index();
        fn set_mode(arg1: ansi::Mode);
        fn unset_mode(arg1: ansi::Mode);
        fn report_mode(arg1: ansi::Mode);
        fn set_private_mode(arg1: ansi::PrivateMode);
        fn unset_private_mode(arg1: ansi::PrivateMode);
        fn report_private_mode(arg1: ansi::PrivateMode);
        fn set_scrolling_region(arg1: usize, arg2: Option<usize>);
        fn set_keypad_application_mode();
        fn unset_keypad_application_mode();
        fn set_active_charset(arg1: ansi::CharsetIndex);
        fn configure_charset(arg1: ansi::CharsetIndex, arg2: ansi::StandardCharset);
        fn set_color(arg1: usize, arg2: ansi::Rgb);
        fn dynamic_color_sequence(arg1: String, arg2: usize, arg3: &str);
        fn reset_color(arg1: usize);
        fn clipboard_store(arg1: u8, arg2: &[u8]);
        fn clipboard_load(arg1: u8, arg2: &str);
        fn decaln();
        fn push_title();
        fn pop_title();
        fn text_area_size_pixels();
        fn text_area_size_chars();
        fn report_keyboard_mode();
        fn push_keyboard_mode(arg1: ansi::KeyboardModes);
        fn pop_keyboard_modes(arg1: u16);
        fn set_keyboard_mode(arg1: ansi::KeyboardModes, arg2: ansi::KeyboardModesApplyBehavior);
    }
}

fn bound_combining(term: &mut Term<Events>, links: &mut LinkBudget) {
    // Bound each parsed character before a later screen switch hides its cell.
    let point = term.grid().cursor.point;
    let preceding =
        (point.column.0 == 0 && point.line.0 > 0).then_some((point.line.0 - 1, term.columns() - 1));
    let points = (point.column.0.saturating_sub(2)..=point.column.0)
        .map(|column| (point.line.0, column))
        .chain(preceding);
    for (line, column) in points {
        let cell = &mut term.grid_mut()[Line(line)][Column(column)];
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
        links.admit(cell);
    }
}

impl Terminal {
    pub fn size(&self) -> TerminalSize {
        self.size
    }

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
            links: LinkBudget::default(),
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
            colors: None,
            passthrough: false,
            rendered: false,
            view_lacks_primary: false,
            parse: Sequence::Ground,
            pending: Vec::new(),
            unframed: None,
            introduction: None,
        }
    }

    pub fn set_passthrough(&mut self) {
        self.passthrough = true;
        self.introduction = Some(Vec::new());
        self.term.set_options(self.config(self.size));
    }

    fn config(&self, size: TerminalSize) -> Config {
        let cells = if self.passthrough || self.rendered {
            SHELL_HISTORY_CELLS
        } else {
            HISTORY_CELLS
        };
        Config {
            scrolling_history: cells / usize::from(size.cols),
            // Clipboard changes belong to a live, consenting view.
            osc52: Osc52::Disabled,
            ..Config::default()
        }
    }

    /// Feed program output. While a passthrough view follows it, that view's own
    /// terminal answers the queries in it, so no reply is returned here.
    pub fn feed_view(&mut self, data: &[u8], viewed: bool) -> Vec<u8> {
        let alternate = self.term.mode().contains(TermMode::ALT_SCREEN);
        let replies = self.feed(data);
        if self.passthrough {
            self.track_pending(data);
        }
        // Leaving the alternate screen shows a primary screen the view never received.
        if self.view_lacks_primary && alternate && !self.term.mode().contains(TermMode::ALT_SCREEN)
        {
            self.unframed = None;
        }
        if let Some(introduction) = &mut self.introduction {
            if introduction.len() + data.len() <= MAX_INTRODUCTION_BYTES {
                // The first view's terminal answers these queries when it shows them.
                introduction.extend_from_slice(data);
                return Vec::new();
            }
            self.introduction = None;
        }
        if self.passthrough {
            match &mut self.unframed {
                Some(bytes) if viewed && bytes.len() + data.len() <= MAX_PASSTHROUGH_BYTES => {
                    bytes.extend_from_slice(data);
                    return Vec::new();
                }
                unframed => *unframed = None,
            }
        }
        replies
    }

    fn track_pending(&mut self, data: &[u8]) {
        for &byte in data {
            let mut state = self.parse;
            if state == Sequence::StringEscape {
                if byte == b'\\' {
                    self.parse = Sequence::Ground;
                    self.pending.clear();
                    continue;
                }
                // The string ended without its terminator; this escape begins another sequence.
                self.pending.clear();
                self.pending.push(0x1b);
                state = Sequence::Escape;
            }
            self.parse = match (state, byte) {
                (Sequence::String(_), 0x1b) => Sequence::StringEscape,
                (Sequence::String(true), 0x07) => Sequence::Ground,
                (_, 0x18 | 0x1a) => Sequence::Ground,
                (Sequence::String(bell), _) => Sequence::String(bell),
                (_, 0x1b) => {
                    self.pending.clear();
                    Sequence::Escape
                }
                (Sequence::Escape, b'[') => Sequence::Control,
                (Sequence::Escape, b']') => Sequence::String(true),
                (Sequence::Escape, b'P' | b'^' | b'_' | b'X') => Sequence::String(false),
                (Sequence::Escape | Sequence::Intermediate, 0x20..=0x2f) => Sequence::Intermediate,
                (Sequence::Escape | Sequence::Intermediate, _) => Sequence::Ground,
                (Sequence::Control, 0x40..=0x7e) => Sequence::Ground,
                (Sequence::Control, _) => Sequence::Control,
                (Sequence::Character(left), 0x80..=0xbf) if left > 1 => {
                    Sequence::Character(left - 1)
                }
                (_, 0xc2..=0xf4) => {
                    self.pending.clear();
                    Sequence::Character(match byte {
                        0xc2..=0xdf => 1,
                        0xe0..=0xef => 2,
                        _ => 3,
                    })
                }
                _ => Sequence::Ground,
            };
            if self.parse == Sequence::Ground {
                self.pending.clear();
            } else if self.pending.len() <= MAX_OSC_PAYLOAD {
                self.pending.push(byte);
            } else {
                // ponytail: a longer string is not carried; its rest then shows as text.
                self.parse = Sequence::Ground;
                self.pending.clear();
            }
        }
    }

    pub fn set_colors(&mut self, colors: TerminalColors) {
        self.colors = Some(colors);
    }

    /// Draw the view from this screen while it is larger than this terminal; follow the
    /// program's bytes again once it fits.
    pub fn set_view_oversized(&mut self, oversized: bool) {
        // Only a shell's view follows bytes; an agent's is always drawn.
        if !(self.passthrough || self.rendered) || self.rendered == oversized {
            return;
        }
        self.rendered = oversized;
        self.passthrough = !oversized;
        // Either way the view is redrawn once for the new way of following.
        self.introduction = None;
        self.unframed = None;
        self.previous = None;
        self.snapshot = None;
        self.dirty = true;
    }

    /// Program output a passthrough view follows but has not taken yet.
    pub fn view_backlog(&self) -> usize {
        if self.passthrough {
            self.unframed.as_ref().map_or(0, Vec::len)
        } else {
            0
        }
    }

    /// Stop holding output for a view that no longer takes it; it is redrawn from this
    /// screen when it next does.
    pub fn drop_view_backlog(&mut self) {
        if self.passthrough && self.unframed.take().is_some() {
            self.dirty = true;
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
        self.parser
            .advance(&mut BoundedTerm(&mut self.term, &mut self.links), data);
    }

    pub fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    pub fn flush_sync(&mut self) -> Vec<u8> {
        self.parser
            .stop_sync(&mut BoundedTerm(&mut self.term, &mut self.links));
        self.dirty = true;
        self.take_replies()
    }

    fn take_replies(&self) -> Vec<u8> {
        let mut replies = Vec::new();
        for event in self.events.replies.lock().unwrap().drain(..) {
            let reply = match event {
                Event::PtyWrite(reply) => reply,
                Event::ColorRequest(index, format) => {
                    format(self.term.colors()[index].unwrap_or_else(|| {
                        let rgb = |[r, g, b]: [u8; 3]| Rgb { r, g, b };
                        match (self.colors, index) {
                            (Some(colors), 0..=15) => colors.palette.map_or_else(
                                || default_color(index),
                                |palette| rgb(palette[index]),
                            ),
                            (Some(colors), 257) => rgb(colors.background),
                            // Matches default_color: cursor and other named colors use foreground.
                            (Some(colors), 256 | 258..) => rgb(colors.foreground),
                            _ => default_color(index),
                        }
                    }))
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

    pub fn resize(&mut self, size: TerminalSize) {
        if self.size != size {
            // Trim old rows before widening them, including the inactive primary grid.
            self.term.set_options(self.config(size));
            self.term.resize(Size(size));
            self.size = size;
            self.dirty = true;
            // A redraw nothing has followed yet was drawn for the old size: draw it again.
            if self.passthrough && self.snapshot.is_some() {
                self.unframed = None;
            }
        }
    }

    pub fn frame(&mut self, after: u64) -> (u64, bool, TerminalSize, Arc<str>) {
        if let Some(mut introduction) = self.introduction.take().filter(|_| self.passthrough) {
            self.dirty = false;
            let rest = introduction.split_off(complete_utf8(&introduction));
            let introduction: Arc<str> = Arc::from(String::from_utf8_lossy(&introduction));
            self.diff = introduction.clone();
            self.snapshot = Some(introduction);
            self.unframed = Some(rest);
            self.sequence += 1;
        } else if self.passthrough && self.dirty {
            self.dirty = false;
            match &mut self.unframed {
                Some(bytes) => {
                    let complete = complete_utf8(bytes);
                    if complete > 0 {
                        self.diff = Arc::from(String::from_utf8_lossy(&bytes[..complete]));
                        bytes.drain(..complete);
                        self.snapshot = None;
                        self.sequence += 1;
                    }
                }
                None => {
                    let projection = self.project();
                    self.view_lacks_primary = projection.alternate;
                    let snapshot: Arc<str> = Arc::from(projection.snapshot());
                    self.diff = snapshot.clone();
                    self.snapshot = Some(snapshot);
                    self.unframed = Some(self.pending.clone());
                    self.sequence += 1;
                }
            }
        } else if self.dirty {
            let next = self.project();
            if self.previous.as_ref() != Some(&next) {
                let diff = self
                    .previous
                    .as_ref()
                    .and_then(|previous| next.diff(previous));
                let full = diff.is_none();
                self.diff = Arc::from(diff.unwrap_or_else(|| next.snapshot()));
                // A full diff is also the reconnect snapshot; retain one allocation.
                self.snapshot = full.then(|| self.diff.clone());
                self.previous = Some(next);
                self.sequence += 1;
            }
            self.dirty = false;
        }
        if after == self.sequence {
            return (self.sequence, false, self.size, Arc::from(""));
        }
        if after != 0 && after.checked_add(1) == Some(self.sequence) {
            return (self.sequence, false, self.size, self.diff.clone());
        }
        if self.snapshot.is_none() {
            let projection = match &self.previous {
                Some(previous) if !self.passthrough => previous.snapshot(),
                _ => {
                    let projection = self.project();
                    if self.passthrough {
                        self.view_lacks_primary = projection.alternate;
                        self.unframed = Some(self.pending.clone());
                    }
                    projection.snapshot()
                }
            };
            self.snapshot = Some(Arc::from(projection));
        }
        (
            self.sequence,
            true,
            self.size,
            self.snapshot.clone().unwrap(),
        )
    }

    pub fn status_signals(&self) -> (String, Option<String>) {
        let mut screen = String::new();
        // Match the GUI's recent live rows; replaced frames stay in history.
        for line in i32::from(self.size.rows).saturating_sub(80).max(0)..i32::from(self.size.rows) {
            for column in 0..usize::from(self.size.cols) {
                let cell = &self.term.grid()[Line(line)][Column(column)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                screen.push(if cell.c.is_control() { ' ' } else { cell.c });
                screen.extend(
                    cell.zerowidth()
                        .unwrap_or_default()
                        .iter()
                        .copied()
                        .take(MAX_COMBINING)
                        .filter(|c| !c.is_control()),
                );
            }
            screen.truncate(screen.trim_end_matches(' ').len());
            screen.push('\n');
        }
        (screen, self.events.title.lock().unwrap().clone())
    }

    fn project(&self) -> Projection {
        let grid = self.term.grid();
        let mut link_bytes = MAX_PROJECTION_LINK_BYTES;
        let columns = usize::from(self.size.cols);
        let wraps = |line: i32| {
            grid[Line(line)][Column(columns - 1)]
                .flags
                .contains(Flags::WRAPLINE)
        };
        let mut row = |line: i32| {
            let mut result = String::new();
            let mut previous: Option<&Cell> = None;
            let mut link = None;
            // A passthrough view's terminal keeps these rows as its own lines: the blank
            // cells after a line's end are not part of it.
            let end = if self.passthrough && !wraps(line) {
                (0..columns)
                    .rev()
                    .find(|column| !blank(&grid[Line(line)][Column(*column)]))
                    .map_or(0, |column| column + 1)
            } else {
                columns
            };
            for column in 0..end {
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
                    if let Some(next_link) = next_link
                        .as_ref()
                        .filter(|link| safe_osc(link.uri()) && link.uri().len() <= link_bytes)
                    {
                        let _ = write!(result, "\x1b]8;;{}\x1b\\", next_link.uri());
                        link_bytes -= next_link.uri().len();
                    }
                    link = next_link;
                }
                glyph(&mut result, cell);
                previous = Some(cell);
            }
            result.push_str("\x1b]8;;\x1b\\\x1b[0m");
            result
        };
        // Reserve links for live rows, then keep the newest history within the byte budget.
        let rows: Vec<_> = (0..i32::from(self.size.rows)).map(&mut row).collect();
        let mut bytes = rows.iter().map(|row| row.len() + 2).sum::<usize>();
        let mut history = Vec::new();
        let mut soft = Vec::new();
        for line in (-(grid.history_size() as i32)..0).rev() {
            let rendered = row(line);
            // Leave room for cursor/mode controls and diff row addresses.
            if bytes + rendered.len() + 2 > MAX_PROJECTION_BYTES - 4096 {
                break;
            }
            bytes += rendered.len() + 2;
            history.push(rendered);
            soft.push(wraps(line));
        }
        history.reverse();
        soft.reverse();
        soft.extend((0..i32::from(self.size.rows)).map(wraps));
        if !self.passthrough {
            soft.clear();
        }
        let cursor = grid.cursor.point;
        let mut trailer = String::from("\x1b[0m");
        // A passthrough view's own terminal carries on from here, so it also needs the state
        // only later output reveals. A rendered view never does: each frame draws it anew.
        if self.passthrough && (0..columns).any(|c| self.term.tab_stop(Column(c)) != (c % 8 == 0)) {
            trailer.push_str("\x1b[3g");
            for column in (0..columns).filter(|c| self.term.tab_stop(Column(*c))) {
                let _ = write!(trailer, "\x1b[1;{}H\x1bH", column + 1);
            }
        }
        if self.passthrough {
            // Colors the program redefined; the view's theme provides the rest.
            for index in 0..alacritty_terminal::term::color::COUNT {
                let Some(Rgb { r, g, b }) = self.term.colors()[index] else {
                    continue;
                };
                let target = match index {
                    0..=255 => format!("4;{index}"),
                    index if index == NamedColor::Foreground as usize => "10".into(),
                    index if index == NamedColor::Background as usize => "11".into(),
                    index if index == NamedColor::Cursor as usize => "12".into(),
                    _ => continue,
                };
                let _ = write!(trailer, "\x1b]{target};rgb:{r:02x}/{g:02x}/{b:02x}\x1b\\");
            }
        }
        let saved = &grid.saved_cursor;
        let saved_cursor = self.passthrough && *saved != Default::default();
        if saved_cursor {
            designate(&mut trailer, &saved.charsets);
            let _ = write!(
                trailer,
                "\x1b[{};{}H",
                saved.point.line.0 + 1,
                saved.point.column.0 + 1
            );
            style(&mut trailer, &saved.template);
            trailer.push_str("\x1b7\x1b[0m");
        }
        // What the program writes next scrolls within its region. Setting one moves the
        // cursor, so it comes first.
        let region = self.term.scroll_region();
        if region.start.0 != 0 || region.end.0 != i32::from(self.size.rows) {
            let _ = write!(trailer, "\x1b[{};{}r", region.start.0 + 1, region.end.0);
        }
        // In origin mode the program addresses rows from the top of its region, and so
        // does the cursor placed here.
        let mut row = cursor.line.0 + 1;
        if self.term.mode().contains(TermMode::ORIGIN) {
            trailer.push_str("\x1b[?6h");
            row -= region.start.0;
        }
        let _ = write!(trailer, "\x1b[{row};{}H", cursor.column.0 + 1);
        for (flag, mode) in [
            // Rows are drawn without wrapping; what the program writes next wraps again.
            (TermMode::LINE_WRAP, 7),
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
        // Past the last column the cursor waits for the next character before it wraps.
        // Writing that column again leaves the view's cursor waiting too.
        // ponytail: the rewritten cell loses its hyperlink; carry it if that is ever seen.
        if self.passthrough
            && grid.cursor.input_needs_wrap
            && self.term.mode().contains(TermMode::LINE_WRAP)
        {
            let line = &grid[cursor.line];
            let mut column = cursor.column.0;
            if column > 0 && line[Column(column)].flags.contains(Flags::WIDE_CHAR_SPACER) {
                column -= 1;
            }
            let _ = write!(trailer, "\x1b[{row};{}H", column + 1);
            style(&mut trailer, &line[Column(column)]);
            glyph(&mut trailer, &line[Column(column)]);
            trailer.push_str("\x1b[0m");
        }
        if self.passthrough {
            if saved_cursor || grid.cursor.charsets != Charsets::default() {
                designate(&mut trailer, &grid.cursor.charsets);
            }
            if self.term.active_charset() == CharsetIndex::G1 {
                trailer.push('\x0e');
            }
            if self.term.mode().contains(TermMode::INSERT) {
                trailer.push_str("\x1b[4h");
            }
        }
        // The program's pen: what it writes next keeps its colors and attributes.
        let pen = &grid.cursor.template;
        if pen.fg != Color::Named(NamedColor::Foreground)
            || pen.bg != Color::Named(NamedColor::Background)
            || !pen.flags.is_empty()
        {
            style(&mut trailer, pen);
        }
        Projection {
            size: self.size,
            alternate: self.term.mode().contains(TermMode::ALT_SCREEN),
            history,
            rows,
            soft,
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
        if self.soft.is_empty() {
            data.push_str("\x1b[?7l\x1b[H");
            self.write_rows(&mut data, &self.history);
        } else {
            // Drawn with wrapping, so a line that wrapped is one line in the view's terminal
            // too: it copies and reflows there as it does here.
            data.push_str("\x1b[?7h\x1b[H");
            for (index, row) in self.history.iter().chain(&self.rows).enumerate() {
                if index != 0 && !self.soft[index - 1] {
                    data.push_str("\r\n");
                }
                data.push_str(row);
            }
        }
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

    fn diff(&self, previous: &Self) -> Option<String> {
        if self.size != previous.size || self.alternate != previous.alternate {
            return None;
        }
        let mut data = String::from("\x1b[?25l\x1b[?7l\x1b[?6l\x1b[r");
        if self.history != previous.history {
            let overlap = history_overlap(&previous.history, &self.history);
            if overlap == 0 && !previous.history.is_empty() {
                return None;
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
        Some(data)
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

fn blank(cell: &Cell) -> bool {
    cell.c == ' '
        && cell.bg == Color::Named(NamedColor::Background)
        && (cell.flags - Flags::WRAPLINE).is_empty()
        && cell.zerowidth().is_none()
        && cell.hyperlink().is_none()
}

fn designate(data: &mut String, charsets: &Charsets) {
    for (index, slot) in [
        (CharsetIndex::G0, '('),
        (CharsetIndex::G1, ')'),
        (CharsetIndex::G2, '*'),
        (CharsetIndex::G3, '+'),
    ] {
        let set = match charsets[index] {
            StandardCharset::Ascii => 'B',
            StandardCharset::SpecialCharacterAndLineDrawing => '0',
        };
        let _ = write!(data, "\x1b{slot}{set}");
    }
}

/// The length of `bytes` without a character split across two reads at its end.
fn complete_utf8(bytes: &[u8]) -> usize {
    match std::str::from_utf8(bytes) {
        Err(error) if error.error_len().is_none() => error.valid_up_to(),
        _ => bytes.len(),
    }
}

fn glyph(data: &mut String, cell: &Cell) {
    data.push(if cell.c.is_control() { ' ' } else { cell.c });
    if let Some(marks) = cell.zerowidth() {
        data.extend(
            marks
                .iter()
                .copied()
                .take(MAX_COMBINING)
                .filter(|c| !c.is_control()),
        );
    }
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

    // The production RSS budgets exclude ASan's shadow memory and allocator quarantine.
    #[cfg(target_os = "linux")]
    const CHECK_RSS: bool = option_env!("FLOWMUX_TEST_ASAN").is_none();

    #[test]
    fn narrowing_history_bounds_peak_memory_and_preserves_both_screens() {
        #[cfg(target_os = "linux")]
        if std::env::var_os("FLOWMUX_TEST_NARROW_MEMORY_CHILD").is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "agent_host::terminal::tests::narrowing_history_bounds_peak_memory_and_preserves_both_screens",
                    "--nocapture",
                ])
                .env("FLOWMUX_TEST_NARROW_MEMORY_CHILD", "1")
                .status()
                .unwrap();
            assert!(
                result.success(),
                "isolated narrowing memory regression failed"
            );
            return;
        }
        #[cfg(target_os = "linux")]
        let peak_kib = || -> usize {
            std::fs::read_to_string("/proc/self/status")
                .unwrap()
                .lines()
                .find(|line| line.starts_with("VmHWM:"))
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap()
                .parse()
                .unwrap()
        };
        #[cfg(target_os = "linux")]
        let before = peak_kib();
        for alternate in [false, true] {
            let mut source = Terminal::new(TerminalSize {
                rows: 20,
                cols: 500,
            });
            let mut row = vec![b'x'; 500];
            row.extend_from_slice(b"\r\n");
            source.feed(&row.repeat(HISTORY_CELLS / 500 + 20));
            source.feed(b"\r\nZ");
            if alternate {
                source.feed(b"\x1b[?1049hA");
            }
            source.resize(TerminalSize { rows: 20, cols: 2 });
            if alternate {
                assert!(visible(&source).concat().contains('A'));
                source.feed(b"\x1b[?1049l");
            }
            assert!(visible(&source).concat().contains('Z'));
            assert!(source.term.grid().history_size() * 2 <= HISTORY_CELLS);
        }
        #[cfg(target_os = "linux")]
        if CHECK_RSS {
            let growth = peak_kib().saturating_sub(before);
            eprintln!("NARROW_PEAK_GROWTH_KIB={growth}");
            assert!(
                growth < 128 * 1024,
                "narrowing exceeded 128 MiB headroom: {growth} KiB"
            );
        }
    }

    #[test]
    fn widening_history_bounds_peak_memory_and_preserves_both_screens() {
        #[cfg(target_os = "linux")]
        if std::env::var_os("FLOWMUX_TEST_RESIZE_MEMORY_CHILD").is_none() {
            // Keep VmHWM independent of other tests and their parallel allocations.
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "agent_host::terminal::tests::widening_history_bounds_peak_memory_and_preserves_both_screens",
                    "--nocapture",
                ])
                .env("FLOWMUX_TEST_RESIZE_MEMORY_CHILD", "1")
                .status()
                .unwrap();
            assert!(result.success(), "isolated resize memory regression failed");
            return;
        }
        #[cfg(target_os = "linux")]
        let peak_kib = || -> usize {
            std::fs::read_to_string("/proc/self/status")
                .unwrap()
                .lines()
                .find(|line| line.starts_with("VmHWM:"))
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap()
                .parse()
                .unwrap()
        };
        #[cfg(target_os = "linux")]
        let before = peak_kib();
        for alternate in [false, true] {
            let mut source = Terminal::new(TerminalSize { rows: 20, cols: 2 });
            source.feed(&b"x\r\n".repeat(HISTORY_CELLS / 2 + 20));
            source.feed(b"\r\nZ");
            assert_eq!(source.term.grid().history_size(), HISTORY_CELLS / 2);
            if alternate {
                source.feed(b"\x1b[?1049hA");
            }
            source.resize(TerminalSize {
                rows: 20,
                cols: 500,
            });
            if alternate {
                assert!(visible(&source).concat().contains('A'));
                source.feed(b"\x1b[?1049l");
            }
            assert!(visible(&source).concat().contains('Z'));
            assert!(source.term.grid().history_size() * 500 <= HISTORY_CELLS);
        }
        #[cfg(target_os = "linux")]
        if CHECK_RSS {
            let growth = peak_kib().saturating_sub(before);
            eprintln!("RESIZE_PEAK_GROWTH_KIB={growth}");
            assert!(
                growth < 128 * 1024,
                "resize exceeded 128 MiB headroom: {growth} KiB"
            );
        }
    }

    #[test]
    fn projection_bounds_repeated_links_without_losing_live_text() {
        let size = TerminalSize::default();
        let mut source = Terminal::new(size);
        let uri = format!("https://example.test/{}", "x".repeat(800));
        let cell = format!("\x1b]8;;{uri}\x1b\\X");
        source.feed(
            cell.repeat(usize::from(size.rows) * usize::from(size.cols))
                .as_bytes(),
        );
        let (_, _, _, data) = source.frame(0);
        assert!(data.matches(&uri).count() * uri.len() <= MAX_PROJECTION_LINK_BYTES);
        assert!(data.len() < 512 * 1024);
        let mut view = Terminal::new(size);
        view.feed(data.as_bytes());
        assert_eq!(visible(&source), visible(&view));
        assert!(view.term.grid()[Line(0)][Column(0)].hyperlink().is_some());
        assert!(view.term.grid()[Line(23)][Column(79)].hyperlink().is_none());
        assert!(source.term.grid()[Line(23)][Column(79)]
            .hyperlink()
            .is_some());
    }

    #[test]
    fn projection_trims_old_history_under_ansi_and_unicode_load() {
        let size = TerminalSize {
            rows: 100,
            cols: 400,
        };
        let mut source = Terminal::new(size);
        let marks = "\u{1d185}".repeat(MAX_COMBINING);
        let mut line = String::new();
        for column in 0..size.cols - 8 {
            let _ = write!(
                line,
                "\x1b[0;1;2;3;4:3;7;8;9;38;2;{};1;2;48;2;3;4;5;58;2;6;7;8mX{marks}",
                column % 255
            );
        }
        line.push_str("\r\n");
        for index in 0..400 {
            source.feed(format!("{index:06}: {line}").as_bytes());
        }
        let (sequence, _, _, data) = source.frame(0);
        assert!(data.len() <= MAX_PROJECTION_BYTES, "{}", data.len());
        assert!(
            source.previous.as_ref().unwrap().history.len() < source.term.grid().history_size()
        );
        let mut view = Terminal::new(size);
        view.feed(data.as_bytes());
        assert_eq!(visible(&source), visible(&view));
        let retained = view.term.grid().history_size() as i32;
        assert!(retained > 0);
        for line in [-retained, -1] {
            for column in 0..8 {
                assert_eq!(
                    source.term.grid()[Line(line)][Column(column)].c,
                    view.term.grid()[Line(line)][Column(column)].c,
                );
            }
        }
        source.feed(b"\x1b[0mnewest output\r\n");
        let (_, _, _, diff) = source.frame(sequence);
        assert!(diff.len() <= MAX_PROJECTION_BYTES);
        view.feed(diff.as_bytes());
        assert_eq!(visible(&source), visible(&view));
    }

    #[test]
    fn maximum_width_and_overlapping_frames_bound_peak_memory() {
        #[cfg(target_os = "linux")]
        if std::env::var_os("FLOWMUX_TEST_MAX_WIDTH_CHILD").is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "agent_host::terminal::tests::maximum_width_and_overlapping_frames_bound_peak_memory",
                    "--nocapture",
                ])
                .env("FLOWMUX_TEST_MAX_WIDTH_CHILD", "1")
                .status()
                .unwrap();
            assert!(result.success(), "isolated maximum width regression failed");
            return;
        }
        // The widest terminal with the most cells: width costs the most memory.
        let size = TerminalSize {
            rows: (TerminalSize::MAX_CELLS / usize::from(TerminalSize::MAX_COLS)) as u16,
            cols: TerminalSize::MAX_COLS,
        }
        .validate()
        .unwrap();
        let mut source = Terminal::new(TerminalSize {
            rows: size.rows - 1,
            cols: size.cols - 1,
        });
        source.feed(&b"x\r\n".repeat(1500));
        source.resize(TerminalSize {
            rows: size.rows - 1,
            cols: size.cols,
        });
        source.resize(size);
        for index in 0..4096 {
            source.feed(format!("\x1b]2;{index:04}{}\x07\x1b[22;0t", "t".repeat(4092)).as_bytes());
        }
        assert!(source.status_signals().1.unwrap().starts_with("4095"));
        let marks = "\u{1d185}".repeat(MAX_COMBINING);
        let mut row = String::new();
        for column in 0..size.cols {
            let _ = write!(
                row,
                "\x1b[0;1;2;3;4:3;7;8;9;38;2;{};1;2;48;2;3;4;5;58;2;6;7;8mX{marks}",
                column % 255
            );
        }
        row.push_str("\r\n");
        for _ in 0..1500 {
            source.feed(row.as_bytes());
        }
        for column in 0..size.cols {
            source.feed(
                format!(
                    "\x1b[1;{}H\x1b]8;;https://example.com/{column:03}/{}\x1b\\X{marks}",
                    column + 1,
                    "l".repeat(4000),
                )
                .as_bytes(),
            );
        }
        source.feed(b"\x1b]8;;\x1b\\");
        assert!(source.links.bytes > 0);
        source.feed(b"\x1b[?1049h");
        for _ in 0..1500 {
            source.feed(row.as_bytes());
        }
        source.feed(b"\x1b[?1049l");
        let first = source.frame(0).3;
        source.resize(TerminalSize {
            rows: size.rows - 1,
            cols: size.cols,
        });
        source.feed(b"\x1b[1;1HA");
        let second = source.frame(0).3;
        assert!(source.diff.starts_with("\x1bc"));
        source.resize(size);
        source.feed(b"\x1b[1;1HB");
        let current = source.frame(0).3;
        assert!(source.diff.starts_with("\x1bc"));
        assert!(first.contains('X') && second.contains('A') && current.contains('B'));
        eprintln!("MAX_WIDTH_PRIMARY_FRAME_BYTES={}", first.len());
        eprintln!("MAX_WIDTH_FRAME_BYTES={}", current.len());
        #[cfg(target_os = "linux")]
        if CHECK_RSS {
            let status = std::fs::read_to_string("/proc/self/status").unwrap();
            let peak: usize = status
                .lines()
                .find(|line| line.starts_with("VmHWM:"))
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap()
                .parse()
                .unwrap();
            eprintln!("MAX_WIDTH_PEAK_KIB={peak}");
            assert!(
                peak < 384 * 1024,
                "terminal alone exceeds execution reservation: {peak} KiB"
            );
        }
    }

    #[test]
    fn retained_link_budget_preserves_text_and_reclaims_cleared_cells() {
        let mut source = Terminal::new(TerminalSize::default());
        let uri = format!("https://retained.example/{}", "x".repeat(4000));
        for index in 0..2000 {
            source.feed(
                format!(
                    "\x1b]8;id={index};{uri}\x1b\\\x1b[58;5;{color}mX\u{301}",
                    color = index % 255
                )
                .as_bytes(),
            );
            assert!(source.links.bytes <= MAX_RETAINED_LINK_BYTES);
        }
        assert!(visible(&source)
            .iter()
            .all(|line| line.starts_with("X\u{301}")));
        assert!(source
            .term
            .grid()
            .display_iter()
            .any(|cell| { cell.c == 'X' && cell.hyperlink().is_none() }));
        assert!(source.links.allocations.len() <= MAX_RETAINED_LINK_BYTES / 256);
        source.feed(b"\x1b[3J\x1b[2J\x1b[H\x1b]8;;https://fresh.example\x1b\\fresh");
        assert_eq!(
            source.term.grid()[Line(0)][Column(0)]
                .hyperlink()
                .unwrap()
                .uri(),
            "https://fresh.example"
        );
        assert!(visible(&source)[0].starts_with("fresh"));
    }

    #[test]
    fn retained_link_budget_keeps_hidden_and_saved_cursor_owners_until_reset() {
        let mut source = Terminal::new(TerminalSize { rows: 20, cols: 20 });
        let uri = format!("https://hidden.example/{}", "x".repeat(4000));
        for index in 0..400 {
            source.feed(format!("\x1b]8;id={index};{uri}\x1b\\X").as_bytes());
        }
        source.feed(b"\x1b7\x1b[?1049h");
        for index in 0..400 {
            source.feed(format!("\x1b]8;id=alt-{index};{uri}\x1b\\A").as_bytes());
        }
        assert!(source.term.grid()[Line(19)][Column(19)]
            .hyperlink()
            .is_none());
        assert!(source.links.bytes <= MAX_RETAINED_LINK_BYTES);
        source.feed(b"\x1b[?1049l\x1b8");
        assert_eq!(
            source.term.grid()[Line(0)][Column(0)]
                .hyperlink()
                .unwrap()
                .uri(),
            uri
        );
        assert!(source.term.grid().cursor.template.hyperlink().is_some());
        source.feed(b"\x1bc");
        assert_eq!(source.links.bytes, 0);
        assert!(source.links.cells.is_empty() && source.links.allocations.is_empty());
        source.feed(b"\x1b]8;;https://after-reset.example\x1b\\new");
        assert_eq!(
            source.term.grid()[Line(0)][Column(0)]
                .hyperlink()
                .unwrap()
                .uri(),
            "https://after-reset.example"
        );
    }

    #[test]
    fn retained_link_budget_tracks_ascii_overwrites_of_wide_spacers() {
        let mut source = Terminal::new(TerminalSize::default());
        source.feed(b"\x1b]8;;https://wide.example\x1b\\");
        source.feed("\u{754c}".as_bytes());
        source.feed(b"\x1b]8;;\x1b\\\x1b[1;2HX");
        let cell = &source.term.grid()[Line(0)][Column(0)];
        assert_eq!(cell.c, ' ');
        assert_eq!(cell.hyperlink().unwrap().uri(), "https://wide.example");
        assert!(source
            .links
            .cells
            .contains(&(Arc::as_ptr(cell.extra.as_ref().unwrap()) as usize)));
        assert!(visible(&source)[0].starts_with(" X"));
    }

    #[test]
    fn titles_are_bounded_in_current_and_saved_titles() {
        let mut terminal = Terminal::new(TerminalSize::default());
        terminal.feed(b"\x1b]2;original\x07\x1b[22;0t\x1b]2;nested\x07\x1b[22;0t");
        terminal.feed(b"\x1b]2;latest\x07\x1b[23;0t");
        assert_eq!(terminal.status_signals().1.as_deref(), Some("nested"));
        terminal.feed(b"\x1b[23;0t");
        assert_eq!(terminal.status_signals().1.as_deref(), Some("original"));
        for index in 0..1200 {
            terminal
                .feed(format!("\x1b]2;{index}{}\x07\x1b[22;0t", "한".repeat(20_000)).as_bytes());
        }
        let title = terminal.status_signals().1.unwrap();
        assert!(title.len() <= 4096);
        assert!(title.starts_with("1199"));
        terminal.feed(b"\x1b[23;0t\x1b[23;0t");
        let title = terminal.status_signals().1.unwrap();
        assert!(title.len() <= MAX_TITLE_BYTES);
        assert!(title.starts_with("1198"));
    }

    #[test]
    fn status_signals_use_live_cells_and_title_without_styles_or_old_frames() {
        let mut terminal = Terminal::new(TerminalSize { rows: 4, cols: 40 });
        terminal.feed(b"old spinner\r\n\x1b]2;Working\x07\x1b[2J\x1b[H");
        terminal.feed("\x1b[31m한글 e\u{301}\x1b[0m\r\nType your message".as_bytes());
        let (screen, title) = terminal.status_signals();
        assert_eq!(screen, "한글 e\u{301}\nType your message\n\n\n");
        assert_eq!(title.as_deref(), Some("Working"));
        terminal.feed(b"\x1b[?1049h\x1b[2J\x1b[HReady\x1b]2;\x07");
        let (screen, title) = terminal.status_signals();
        assert_eq!(screen, "Ready\n\n\n\n");
        assert!(title.as_deref().is_none_or(str::is_empty));
    }

    #[test]
    fn status_signals_bound_recent_rows_and_wait_for_synchronized_output() {
        let mut terminal = Terminal::new(TerminalSize {
            rows: 100,
            cols: 40,
        });
        terminal.feed(b"old spinner\x1b[100;1HType your message");
        let (screen, _) = terminal.status_signals();
        assert_eq!(screen.lines().count(), 80);
        assert!(!screen.contains("spinner"));
        assert!(screen.contains("Type your message"));
        terminal.feed(b"\x1b[?2026h\x1b[2J\x1b[Hnew frame");
        assert!(!terminal.status_signals().0.contains("new frame"));
        terminal.flush_sync();
        // The first row is outside the 80-row observation window.
        assert!(!terminal.status_signals().0.contains("new frame"));
        assert!(!terminal.status_signals().0.contains("Type your message"));
    }

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
    fn color_queries_use_view_colors_until_the_application_overrides_them() {
        let queries = b"\x1b]10;?\x07\x1b]11;?\x07\x1b]4;1;?\x07\x1b]4;100;?\x07";
        let mut terminal = Terminal::new(TerminalSize::default());
        assert_eq!(
            terminal.feed(queries),
            b"\x1b]10;rgb:e5e5/e5e5/e5e5\x07\x1b]11;rgb:0000/0000/0000\x07\
              \x1b]4;1;rgb:cdcd/0000/0000\x07\x1b]4;100;rgb:8787/8787/0000\x07"
        );
        let (sequence, ..) = terminal.frame(0);
        let mut palette = [[0; 3]; 16];
        palette[1] = [0xdc, 0x32, 0x2f];
        terminal.set_colors(TerminalColors {
            foreground: [0x65, 0x7b, 0x83],
            background: [0xfd, 0xf6, 0xe3],
            palette: Some(palette),
        });
        // New colors alone neither advance nor rebuild the rendered frame.
        assert_eq!(
            terminal.frame(sequence),
            (sequence, false, terminal.size, Arc::from(""))
        );
        assert_eq!(
            terminal.feed(queries),
            b"\x1b]10;rgb:6565/7b7b/8383\x07\x1b]11;rgb:fdfd/f6f6/e3e3\x07\
              \x1b]4;1;rgb:dcdc/3232/2f2f\x07\x1b]4;100;rgb:8787/8787/0000\x07"
        );
        terminal.set_colors(TerminalColors {
            foreground: [1, 2, 3],
            background: [4, 5, 6],
            palette: None,
        });
        assert_eq!(
            terminal.feed(b"\x1b]11;rgb:10/20/30\x07\x1b]11;?\x07\x1b]4;1;?\x07\x1b]12;?\x07"),
            b"\x1b]11;rgb:1010/2020/3030\x07\x1b]4;1;rgb:cdcd/0000/0000\x07\x1b]12;rgb:0101/0202/0303\x07"
        );
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
    fn oversized_hyperlinks_are_not_retained_in_hidden_cells_or_cursor_templates() {
        let size = TerminalSize { rows: 3, cols: 12 };
        for (id, uri) in [
            (
                "short".into(),
                format!("https://example.com/{}", "x".repeat(4096)),
            ),
            ("x".repeat(4097), "https://example.com".into()),
        ] {
            let mut source = Terminal::new(size);
            source.feed(
                format!("\x1b[?2026h\x1b]8;id={id};{uri}\x1b\\linked\x1b[?1049hhidden\x1b[?2026l")
                    .as_bytes(),
            );
            source.feed(b"\x1b[?1049l");
            assert!(source.term.grid()[Line(0)][Column(0)].hyperlink().is_none());
            assert!(source.term.grid().cursor.template.hyperlink().is_none());
            source.feed(b"\x1b]8;id=valid;https://example.com\x1b\\ok\x1b]8;;\x1b\\");
            assert_eq!(
                source.term.grid()[Line(0)][Column(6)]
                    .hyperlink()
                    .unwrap()
                    .uri(),
                "https://example.com"
            );
            assert!(source.term.grid().cursor.template.hyperlink().is_none());
            assert!(visible(&source)[0].starts_with("linkedok"));
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
    fn combining_bound_survives_sync_release_hidden_screens_and_repeat() {
        for timeout in [false, true] {
            let mut source = Terminal::new(TerminalSize { rows: 3, cols: 12 });
            let output = format!(
                "\x1b[?2026he{}\x1b[?1049halt{}",
                "\u{301}".repeat(1000),
                if timeout { "" } else { "\x1b[?2026l" },
            );
            source.feed(output.as_bytes());
            if timeout {
                source.flush_sync();
            }
            assert!(visible(&source)[0].contains("alt"));
            source.feed(b"\x1b[?1049l");
            let cell = &source.term.grid()[Line(0)][Column(0)];
            assert_eq!(cell.c, 'e');
            assert_eq!(cell.zerowidth().unwrap().len(), MAX_COMBINING);
            source.feed("\re\u{301}\x1b[1000b".as_bytes());
            assert_eq!(
                source.term.grid()[Line(0)][Column(0)]
                    .zerowidth()
                    .unwrap()
                    .len(),
                MAX_COMBINING,
            );
        }
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
    fn passthrough_view_gets_program_bytes_and_is_redrawn_after_missing_output() {
        let mut terminal = Terminal::new(TerminalSize { rows: 4, cols: 20 });
        terminal.set_passthrough();
        terminal.feed_view(b"before\r\n", false);
        let (first, snapshot, _, data) = terminal.frame(0);
        assert!(snapshot && data.contains("before"));
        // The view's own terminal answers the query and keeps the cwd report.
        let output = "\x1b[c\x1b]7;file:///tmp\x07line\r\n";
        assert!(terminal.feed_view(output.as_bytes(), true).is_empty());
        let (second, snapshot, _, data) = terminal.frame(first);
        assert!(!snapshot);
        assert_eq!(&*data, output);
        let wide = "\u{d55c}".as_bytes();
        terminal.feed_view(&wide[..2], true);
        assert_eq!(terminal.frame(second).0, second, "half a character waits");
        terminal.feed_view(&wide[2..], true);
        let (third, _, _, data) = terminal.frame(second);
        assert_eq!(&*data, "\u{d55c}");
        // Output nobody viewed is answered here and reaches the view as a redraw.
        assert!(!terminal.feed_view(b"\x1b[c", false).is_empty());
        terminal.feed_view(b"later", true);
        let (_, _, _, data) = terminal.frame(third);
        assert!(data.contains("before") && data.contains("later"));
        assert!(
            data.rfind("\x1b[?7h") > data.rfind("\x1b[?7l"),
            "output after a redraw wraps again"
        );
        // A program's scrolling region survives the redraw, set before the cursor is placed.
        terminal.feed_view(b"\x1b[2;3r\x1b[3;1H", false);
        let (_, _, _, data) = terminal.frame(0);
        assert!(
            data.ends_with("\x1b[2;3r\x1b[3;1H") || data.contains("\x1b[2;3r\x1b[3;1H\x1b[?7h")
        );
    }

    #[test]
    fn an_oversized_shell_view_is_drawn_from_the_screen_until_it_fits() {
        let mut terminal = Terminal::new(TerminalSize { rows: 4, cols: 10 });
        terminal.set_passthrough();
        terminal.feed_view(b"$ ", false);
        let (first, ..) = terminal.frame(0);
        // The view is wider than the Host: raw bytes would wrap at a different column there.
        terminal.set_view_oversized(true);
        let long = "0123456789abcdef";
        terminal.feed_view(long.as_bytes(), true);
        let (second, snapshot, _, data) = terminal.frame(first);
        assert!(second > first);
        assert_ne!(&*data, long, "an oversized view gets no raw bytes");
        // Rows are placed by the Host without the view wrapping them, so the view shows the
        // Host's wrap and cursor. The first such frame redraws the whole view.
        let _ = snapshot;
        assert!(data.starts_with("\x1bc"), "{data:?}");
        assert!(data.contains("\x1b[?7l\x1b[H"), "{data:?}");
        assert!(data.contains("\r\n\x1b[0;39;49m89abcdef"), "{data:?}");
        assert!(data.contains("\x1b[2;9H"), "{data:?}");
        terminal.feed_view(b"Z", true);
        let (third, _, _, data) = terminal.frame(second);
        assert_ne!(&*data, "Z");
        // Fitting again, the view follows the program's bytes after one redraw.
        terminal.set_view_oversized(false);
        terminal.feed_view(b"Y", true);
        // Delivered as the next frame, but a whole redraw that wraps as passthrough does.
        let (fourth, _, _, data) = terminal.frame(third);
        assert!(data.starts_with("\x1bc\x1b[?7h"), "{data:?}");
        assert!(data.contains("89abcdefZ"), "{data:?}");
        terminal.feed_view(b"X", true);
        let (_, snapshot, _, data) = terminal.frame(fourth);
        assert!(!snapshot);
        assert_eq!(&*data, "X");
    }

    #[test]
    fn passthrough_redraw_keeps_the_pen_and_the_primary_screen() {
        let mut terminal = Terminal::new(TerminalSize { rows: 4, cols: 20 });
        terminal.set_passthrough();
        terminal.feed_view(b"shell\r\n", false);
        terminal.frame(0);
        // Drawn while a program uses the alternate screen, with red text half written.
        terminal.feed_view(b"\x1b[?1049hALT\x1b[31m", false);
        let (sequence, snapshot, _, data) = terminal.frame(0);
        assert!(snapshot && data.contains("ALT") && !data.contains("shell"));
        assert!(data.ends_with("\x1b[0;38;5;1;49m"), "{data:?}");
        // Output that stays on the alternate screen is passed through as it is.
        terminal.feed_view(b"more", true);
        let (sequence, snapshot, _, data) = terminal.frame(sequence);
        assert!(!snapshot && &*data == "more");
        // The view has no primary screen of its own to return to: it is drawn again.
        terminal.feed_view(b"\x1b[0m\x1b[?1049l", true);
        let (sequence, _, _, data) = terminal.frame(sequence);
        assert!(
            data.starts_with("\x1bc") && data.contains("shell"),
            "{data:?}"
        );
        // From then on its own terminal keeps both screens.
        terminal.feed_view(b"\x1b[?1049hagain\x1b[?1049l", true);
        let (_, snapshot, _, data) = terminal.frame(sequence);
        assert!(!snapshot && data.contains("again") && !data.starts_with("\x1bc"));
    }

    /// A plain terminal fed what a passthrough view receives shows what the Host has.
    #[test]
    fn passthrough_view_matches_the_host_after_a_redraw() {
        let size = TerminalSize { rows: 4, cols: 20 };
        for (case, before, after) in [
            (
                "the cursor waits past the last column for the next character",
                b"xxxxxxxxxxxxxxxxxxxx".as_slice(),
                b"Y".as_slice(),
            ),
            (
                "a wide character fills the last two columns",
                "xxxxxxxxxxxxxxxxxx\u{d55c}".as_bytes(),
                b"Y",
            ),
            (
                "cursor addresses are relative to the scrolling region",
                b"\x1b[2;3r\x1b[?6h",
                b"\x1b[1;1HZ\x1b[2;1HW",
            ),
            (
                "the saved cursor keeps its place and its pen",
                b"\x1b[31m\x1b[2;5H\x1b7\x1b[0m\x1b[1;1H",
                b"\x1b8Z",
            ),
            (
                "typed characters are inserted",
                b"abcdef\x1b[4h\x1b[1;1H",
                b"XY",
            ),
            ("line drawing is designated", b"\x1b(0", b"qqxx"),
            ("line drawing is shifted in", b"\x1b)0\x0e", b"qqxx"),
            (
                "tab stops are the program's",
                b"\x1b[3g\x1b[1;5H\x1bH\x1b[1;1H",
                b"a\tb\tc",
            ),
            (
                "the program's palette is kept",
                b"\x1b]4;1;rgb:00/ff/00\x07\x1b]10;#112233\x07\x1b]11;#445566\x07",
                b"\x1b[31mX",
            ),
            ("a control sequence is split", b"ab\x1b[31", b"mX"),
            ("a title is split", b"ab\x1b]0;ti", b"tle\x07X"),
            (
                "an escape is split after its first byte",
                b"ab\x1b",
                b"[31mX",
            ),
            (
                "a character is split",
                &"ab\u{d55c}".as_bytes()[..4],
                &"\u{d55c}X".as_bytes()[2..],
            ),
        ] {
            let mut host = Terminal::new(size);
            host.set_passthrough();
            host.feed_view(b"\r", false);
            host.frame(0);
            host.feed_view(before, false);
            let (sequence, snapshot, _, data) = host.frame(0);
            assert!(snapshot, "{case}");
            let mut view = Terminal::new(size);
            // Rows are compared as both draw them.
            view.set_passthrough();
            view.feed(data.as_bytes());
            host.feed_view(after, true);
            view.feed(host.frame(sequence).3.as_bytes());
            assert_eq!(view.status_signals().0, host.status_signals().0, "{case}");
            assert_eq!(view.project().rows, host.project().rows, "{case}");
            assert_eq!(
                view.term.grid().cursor.point,
                host.term.grid().cursor.point,
                "{case}"
            );
            for index in 0..alacritty_terminal::term::color::COUNT {
                assert_eq!(
                    view.term.colors()[index],
                    host.term.colors()[index],
                    "{case}"
                );
            }
        }
        // A line that wrapped stays one line for the view, and a short line ends where it ends.
        let mut host = Terminal::new(size);
        host.set_passthrough();
        host.feed_view(b"\r", false);
        host.frame(0);
        host.feed_view(format!("L1\r\n{}\r\nend", "w".repeat(30)).as_bytes(), false);
        let data = host.frame(0).3;
        assert!(!data.contains("L1 "), "{data:?}");
        let mut view = Terminal::new(size);
        view.feed(data.as_bytes());
        let wide = TerminalSize { rows: 4, cols: 40 };
        host.resize(wide);
        view.resize(wide);
        assert_eq!(view.status_signals().0, host.status_signals().0);
        assert!(host.status_signals().0.contains(&"w".repeat(30)));
        // A view that attaches at another size is drawn for that size.
        let mut host = Terminal::new(size);
        host.set_passthrough();
        host.feed_view(b"\r", false);
        host.frame(0);
        host.feed_view("w".repeat(30).as_bytes(), false);
        host.frame(0);
        host.resize(wide);
        let (sequence, _, _, data) = host.frame(0);
        let mut view = Terminal::new(wide);
        view.set_passthrough();
        view.feed(data.as_bytes());
        host.feed_view(b"Z", true);
        view.feed(host.frame(sequence).3.as_bytes());
        assert_eq!(view.status_signals().0, host.status_signals().0);
        assert_eq!(view.term.grid().cursor.point, host.term.grid().cursor.point);
        // Wherever a redraw falls in a stream, the rest of it means the same to the view.
        let stream = "a\x1b[31mb\x1b]0;t\x07c\x1b]2;u\x1b\\d\x1bPq\x1b\\e\x1b(0q\x1b(Bf\u{d55c}g\x1b[0mh\r\n\x1b7\x1b[4hi\x1b8j"
            .as_bytes();
        for split in 0..=stream.len() {
            let mut host = Terminal::new(size);
            host.set_passthrough();
            host.feed_view(b"\r", false);
            host.frame(0);
            host.feed_view(&stream[..split], false);
            let (sequence, _, _, data) = host.frame(0);
            let mut view = Terminal::new(size);
            // Rows are compared as both draw them.
            view.set_passthrough();
            view.feed(data.as_bytes());
            host.feed_view(&stream[split..], true);
            view.feed(host.frame(sequence).3.as_bytes());
            assert_eq!(view.project().rows, host.project().rows, "split at {split}");
            assert_eq!(
                view.term.grid().cursor.point,
                host.term.grid().cursor.point,
                "split at {split}"
            );
        }
        // A character split across two reads is not lost with the first frame.
        let mut host = Terminal::new(size);
        host.set_passthrough();
        let split = "a\u{d55c}".as_bytes();
        host.feed_view(&split[..2], false);
        let (sequence, _, _, data) = host.frame(0);
        assert_eq!(&*data, "a");
        host.feed_view(&split[2..], true);
        assert_eq!(&*host.frame(sequence).3, "\u{d55c}");
    }

    #[test]
    fn full_frame_diff_and_reconnect_share_storage() {
        let size = TerminalSize { rows: 3, cols: 12 };
        let mut source = Terminal::new(size);
        source.feed(b"first\r\nsecond\r\nthird\r\nfourth");
        let (mut sequence, _, _, first) = source.frame(0);
        assert!(Arc::ptr_eq(&first, &source.diff));
        for (output, resized) in [
            (b"\x1b[?1049hALT".as_slice(), false),
            (b"\x1b[?1049l".as_slice(), false),
            (b"".as_slice(), true),
            (b"\x1b[3J".as_slice(), false),
        ] {
            source.feed(output);
            if resized {
                source.resize(TerminalSize { rows: 3, cols: 13 });
            }
            let (next, _, current_size, diff) = source.frame(sequence);
            assert!(next > sequence);
            assert!(diff.starts_with("\x1bc"));
            let snapshot = source.frame(0).3;
            assert!(Arc::ptr_eq(&diff, &snapshot));
            let mut view = Terminal::new(current_size);
            view.feed(snapshot.as_bytes());
            assert_eq!(visible(&source), visible(&view));
            sequence = next;
        }
        assert!(first.contains("first"));
        source.feed(b"\x1b[1;1Hchanged");
        let (next, _, _, diff) = source.frame(sequence);
        assert!(next > sequence && !diff.starts_with("\x1bc"));
        let snapshot = source.frame(0).3;
        assert!(!Arc::ptr_eq(&diff, &snapshot));
        assert!(source.frame(next).3.is_empty());
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
