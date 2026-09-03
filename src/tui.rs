//! TUI shell: widget list + overlay callback + mouse selection.
//!
//! Mouse capture is enabled for:
//! - Click-drag text selection in the log area (character, word, line, block modes)
//! - Scrollbar dragging (click + drag to scroll)
//! - Scroll wheel for scrolling
//! - Selection persists and extends during scroll/auto-scroll
//!
//! Selection is copied to clipboard via OSC 52 on mouse release.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::console::Console;
use crate::connection::{SocketConnection, TypedConnection};
use crate::protocol::Protocol;
use tui_input::Input;

/// Name constants for servatui's built-in widgets.
pub const WIDGET_LOG: &str = "servatui.log";
pub const WIDGET_INPUT: &str = "servatui.input";
pub const WIDGET_SCROLLBAR: &str = "servatui.scrollbar";

/// A named, positioned widget in the render list.
pub struct WidgetEntry {
    pub name: &'static str,
    pub widget: Box<dyn ratatui::widgets::WidgetRef>,
    pub area: ratatui::layout::Rect,
}

/// Log lines, scroll state, and selection state.
///
/// Selection is stored in **content coordinates** (absolute wrapped-line row),
/// not viewport coordinates. This means selection survives scrolling —
/// it stays on the same content even when the viewport moves.
pub struct TuiState {
    pub log_lines: Vec<String>,
    pub scroll_up: u16,
    /// Selection in content (absolute) coordinates.
    /// (start_row, start_col, end_row, end_col). None = no selection.
    pub selection: Option<(usize, usize, usize, usize)>,
    /// Whether the selection is rectangular (Ctrl+drag block select).
    pub selection_rect: bool,
    /// Command history (entered lines), for Ctrl+Up/Ctrl+Down recall.
    pub history: Vec<String>,
    /// Current position in `history` during recall; `None` = not recalling.
    pub history_idx: Option<usize>,
}

impl Default for TuiState {
    fn default() -> Self { Self::new() }
}

impl TuiState {
    pub fn new() -> Self {
        Self {
            log_lines: vec!["servatui interactive mode. Type 'help' for commands.".into()],
            scroll_up: 0,
            selection: None,
            selection_rect: false,
            history: Vec::new(),
            history_idx: None,
        }
    }
}

/// Selection mode based on click count.
#[derive(Clone, Copy, PartialEq)]
enum ClickMode { Char, Word, Line }

/// Tracks mouse interaction state across frames.
///
/// Public for the fuzz harnesses (construct via [`MouseState::default`] and
/// drive it with [`handle_mouse_event`]); the fields stay private.
/// Not stable API.
pub struct MouseState {
    selecting: bool,
    scrollbar_drag: bool,
    click_count: u32,
    last_click_time: Instant,
    last_click_pos: (u16, u16),
    click_mode: ClickMode,
    /// Anchor position in content coordinates (where the drag started).
    anchor: Option<(usize, usize)>,
    /// Auto-scroll direction during drag past viewport edge.
    /// None = no auto-scroll, Some(true) = scroll up, Some(false) = scroll down.
    auto_scroll: Option<bool>,
}

impl Default for MouseState {
    fn default() -> Self {
        Self {
            selecting: false,
            scrollbar_drag: false,
            click_count: 0,
            last_click_time: Instant::now() - Duration::from_secs(10),
            last_click_pos: (0, 0),
            click_mode: ClickMode::Char,
            anchor: None,
            auto_scroll: None,
        }
    }
}

/// Characters that are part of a "word" (matching VTE/gnome-terminal defaults).
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || "-./:%_@~".contains(c)
}

/// Find word boundaries around col in the given line.
/// If col points at a non-word character, selects just that character.
fn word_boundaries(line: &str, col: usize) -> (usize, usize) {
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() || col >= chars.len() {
        return (col, col);
    }
    // Non-word char: select just this character
    if !is_word_char(chars[col]) {
        return (col, col + 1);
    }
    let mut start = col;
    let mut end = col;
    while start > 0 && is_word_char(chars[start - 1]) {
        start -= 1;
    }
    while end < chars.len() && is_word_char(chars[end]) {
        end += 1;
    }
    (start, end)
}

/// Read terminal size from COLUMNS and LINES environment variables.
/// Returns None if either is missing, invalid, or zero.
/// Inside nested containers (podman inside toolbox), ioctl(TIOCGWINSZ)
/// returns the intermediate PTY's size. The shell updates $COLUMNS/$LINES
/// on SIGWINCH, and these propagate correctly through container boundaries.
fn terminal_size_from_env() -> Option<(u16, u16)> {
    let cols = std::env::var("COLUMNS").ok()?;
    let lines = std::env::var("LINES").ok()?;
    parse_terminal_size(&cols, &lines)
}

/// Parse COLUMNS and LINES strings into a size. Returns None if invalid or zero.
fn parse_terminal_size(cols: &str, lines: &str) -> Option<(u16, u16)> {
    let w: u16 = cols.parse().ok()?;
    let h: u16 = lines.parse().ok()?;
    if w == 0 || h == 0 { return None; }
    Some((w, h))
}

/// Query the terminal emulator directly with `CSI 18 t`. The reply
/// (`CSI 8 ; cols ; rows t`) carries the *true* window size from the outermost
/// emulator, bypassing a frozen kernel PTY (e.g. podman-in-toolbox where
/// `ioctl(TIOCGWINSZ)` never updates after start). Returns `None` if the
/// terminal doesn't answer within ~15 ms. Done while crossterm is idle (top of
/// the render loop, before its `event::read`), so it can't swallow the reply.
#[cfg(unix)]
fn query_csi_18t() -> Option<(u16, u16)> {
    use std::io::{Read, Write};
    use std::os::unix::io::AsRawFd;

    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()?;
    tty.write_all(b"\x1b[18t").ok()?;
    let _ = tty.flush();

    // Wait briefly for the reply (terminals answer in a few ms).
    let mut pfd = libc::pollfd { fd: tty.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    if unsafe { libc::poll(&mut pfd, 1, 15) } <= 0 {
        return None;
    }

    let mut buf = [0u8; 32];
    let n = tty.read(&mut buf).ok()?;
    let resp = std::str::from_utf8(&buf[..n]).ok()?;

    // Reply: CSI 8 ; <rows> ; <cols> t  (xterm reports height before width).
    let inner = resp.strip_prefix("\x1b[8;")?.strip_suffix('t')?;
    let mut it = inner.split(';');
    let rows: u16 = it.next()?.parse().ok()?;
    let cols: u16 = it.next()?.parse().ok()?;
    Some((cols, rows))
}

/// Throttled `CSI 18 t` result — re-queries at most every 500 ms so the render
/// loop isn't stalled each frame by the synchronous terminal round-trip.
#[cfg(unix)]
fn csi_size_cached() -> Option<(u16, u16)> {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    type CsiCache = Option<(Instant, Option<(u16, u16)>)>;
    static CACHE: Mutex<CsiCache> = Mutex::new(None);
    let mut guard = CACHE.lock().unwrap();
    let now = Instant::now();
    let stale = matches!(*guard, Some((t, _)) if now.duration_since(t) >= Duration::from_millis(500));
    if stale || guard.is_none() {
        *guard = Some((now, query_csi_18t()));
    }
    (*guard).and_then(|(_, s)| s)
}

/// Detect terminal size. Prefers a direct `CSI 18 t` query of the terminal
/// emulator (which reflects live resizes even when the kernel PTY is frozen in
/// nested containers), then falls back to the ioctl via crossterm, then env.
fn detect_terminal_size() -> (u16, u16) {
    #[cfg(unix)]
    if let Some(size) = csi_size_cached() {
        return size;
    }
    crossterm::terminal::size()
        .ok()
        .filter(|&(w, h)| w > 0 && h > 0)
        .or_else(terminal_size_from_env)
        .unwrap_or((80, 24))
}

/// A ratatui backend wrapper that reports [`detect_terminal_size`] from its
/// `size()`. ratatui's per-draw `autoresize()` reads the backend size to size
/// its buffer — so by reporting the `CSI 18 t` size here (instead of the
/// crossterm `ioctl`, which is frozen in nested containers), the whole frame
/// resizes correctly. Every other call delegates to the inner backend.
#[cfg(feature = "tui")]
struct SizeOverrideBackend<B> {
    inner: B,
}

#[cfg(feature = "tui")]
impl<B: ratatui::backend::Backend> ratatui::backend::Backend for SizeOverrideBackend<B> {
    fn draw<'a, I>(&mut self, content: I) -> std::io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.inner.draw(content)
    }
    fn append_lines(&mut self, n: u16) -> std::io::Result<()> { self.inner.append_lines(n) }
    fn hide_cursor(&mut self) -> std::io::Result<()> { self.inner.hide_cursor() }
    fn show_cursor(&mut self) -> std::io::Result<()> { self.inner.show_cursor() }
    fn get_cursor_position(&mut self) -> std::io::Result<ratatui::layout::Position> {
        self.inner.get_cursor_position()
    }
    fn set_cursor_position<P: Into<ratatui::layout::Position>>(&mut self, pos: P) -> std::io::Result<()> {
        self.inner.set_cursor_position(pos)
    }
    fn clear(&mut self) -> std::io::Result<()> { self.inner.clear() }
    fn clear_region(&mut self, clear_type: ratatui::backend::ClearType) -> std::io::Result<()> {
        self.inner.clear_region(clear_type)
    }
    fn size(&self) -> std::io::Result<ratatui::layout::Size> {
        let (w, h) = detect_terminal_size();
        Ok(ratatui::layout::Size::new(w, h))
    }
    fn window_size(&mut self) -> std::io::Result<ratatui::backend::WindowSize> {
        self.inner.window_size()
    }
    fn flush(&mut self) -> std::io::Result<()> { self.inner.flush() }
}

/// Copy text to clipboard via OSC 52 escape sequence.
/// No-op when stdout is not a terminal (e.g. piped output, test harnesses).
fn osc52_copy(text: &str) {
    use base64::{Engine, engine::general_purpose};
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return;
    }
    let encoded = general_purpose::STANDARD.encode(text);
    let _ = write!(std::io::stdout(), "\x1b]52;c;{}\x1b\\", encoded);
    let _ = std::io::stdout().flush();
}

/// Paste from clipboard via OSC 52 read request.
/// The terminal responds with the clipboard content, which arrives as terminal
/// input. This sends the request; the response needs to be handled separately.
/// No-op when stdout is not a terminal.
fn osc52_paste() {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return;
    }
    let _ = write!(std::io::stdout(), "\x1b]52;c;?\x1b\\");
    let _ = std::io::stdout().flush();
}

/// Extract selected text from all wrapped lines (content coordinates).
/// Returns `(content, indent_len)`: the row with any "↪ " continuation indent
/// stripped, plus the number of display columns that indent occupied.
/// Selection columns are display coordinates (they include the indent), so
/// callers must subtract `indent_len` to index into `content`.
fn row_content(row: &str) -> (&str, usize) {
    const INDENT: &str = "↪ ";
    match row.strip_prefix(INDENT) {
        Some(content) => (content, INDENT.chars().count()),
        None => (row, 0),
    }
}

/// Wrap `line` to `width` columns, prefixing continuation rows with `indent`,
/// returning `(display_row, offset)` pairs where `offset` is the character
/// position within `line` at which the row's content begins.
///
/// Runs the same public textwrap pipeline `textwrap::wrap` uses
/// (`find_words` → `split_words` → `break_words` → wrap algorithm), so the
/// rows are byte-identical to `wrap`'s output; the word slices carry exact
/// source positions, which `wrap`'s returned strings do not.
#[cfg(feature = "tui")]
fn wrap_with_offsets(line: &str, width: usize, indent: &str) -> Vec<(String, usize)> {
    use textwrap::core::{break_words, display_width};
    use textwrap::word_splitters::split_words;
    use textwrap::{WordSeparator, WordSplitter, WrapAlgorithm};

    let line_widths = [width, width.saturating_sub(display_width(indent))];
    let separator = WordSeparator::new();
    let splitter = WordSplitter::HyphenSplitter;
    let words = split_words(separator.find_words(line), &splitter);
    let broken = break_words(words, line_widths[1]);
    let groups = WrapAlgorithm::new().wrap(&broken, &line_widths);

    let base = line.as_ptr() as usize;
    let mut rows = Vec::new();
    for (i, group) in groups.iter().enumerate() {
        let (start, end) = match (group.first(), group.last()) {
            (Some(f), Some(l)) => (
                f.word.as_ptr() as usize - base,
                l.word.as_ptr() as usize - base + l.word.len(),
            ),
            _ => {
                // Empty group (e.g. leading-whitespace lines): an empty row.
                rows.push((String::new(), 0));
                continue;
            }
        };
        let content = &line[start..end];
        let offset = line[..start].chars().count();
        let display = if i == 0 {
            content.to_string()
        } else {
            format!("{indent}{content}")
        };
        rows.push((display, offset));
    }
    if rows.is_empty() {
        rows.push((String::new(), 0));
    }
    rows
}

/// Original-line character column for a display `(row, col)` position: the
/// row's content offset plus the column within the row's content (the "↪ "
/// indent is not part of the content). Rectangular selection columns live in
/// these original-line coordinates.
fn orig_col(wrapped: &[String], offsets: &[usize], row: usize, display_col: usize) -> usize {
    let indent = wrapped.get(row).map(|w| row_content(w).1).unwrap_or(0);
    offsets.get(row).copied().unwrap_or(0) + display_col.saturating_sub(indent)
}

/// Strips ↪ prefixes and reinserts the space consumed at each wrap, so the
/// copied text matches the original line. Continuation rows (same original
/// line) are joined with a single space; different original lines are
/// separated by a newline.
pub fn extract_selection(
    wrapped: &[String],
    offsets: &[usize],
    sr: usize, sc: usize, er: usize, ec: usize,
    rect: bool,
) -> String {
    let (sr, sc, er, ec) = if (sr, sc) > (er, ec) { (er, ec, sr, sc) } else { (sr, sc, er, ec) };
    let mut result = String::new();

    if rect {
        // Rectangular selection is content-based: the columns are positions in
        // the ORIGINAL logical line, not screen columns. Each row in the span
        // contributes the characters whose original position falls in
        // [cmin, cmax); rows that don't reach the column contribute a blank
        // line, preserving the box's shape.
        let (cmin, cmax) = (sc.min(ec), sc.max(ec));
        for row in sr..=er {
            if row >= wrapped.len() { break; }
            let (content, _) = row_content(&wrapped[row]);
            let chars: Vec<char> = content.chars().collect();
            let off = offsets.get(row).copied().unwrap_or(0);
            let start = cmin.max(off);
            let end = cmax.min(off + chars.len());
            if start < end {
                result.extend(&chars[start - off..end - off]);
            }
            if row < er {
                result.push('\n');
            }
        }
    } else {
        for row in sr..=er {
            if row >= wrapped.len() { break; }
            let (content, indent) = row_content(&wrapped[row]);
            let chars: Vec<char> = content.chars().collect();
            let col_start = if row == sr { sc.saturating_sub(indent).min(chars.len()) } else { 0 };
            let col_end = if row == er { ec.saturating_sub(indent).min(chars.len()) } else { chars.len() };
            if col_start < col_end { result.extend(&chars[col_start..col_end]); }
            if row < er {
                // A continuation row is the same original line, wrapped: reinsert
                // the space textwrap consumed at the break. A non-continuation row
                // is a different original line → newline.
                let next_cont = wrapped.get(row + 1).map(|l| l.starts_with("↪ ")).unwrap_or(false);
                result.push(if next_cont { ' ' } else { '\n' });
            }
        }
    }
    result
}

/// A widget that renders pre-wrapped log lines with optional selection highlight.
///
/// Public (with public fields) for the fuzz harnesses; not stable API.
pub struct LogWidget {
    pub lines: Vec<String>,
    /// Selection in content coordinates.
    pub selection: Option<(usize, usize, usize, usize)>,
    /// Content row of the first visible line (viewport offset).
    pub viewport_start: usize,
    /// Whether selection is rectangular.
    pub rect: bool,
    /// Per content row: char offset into its original line (content coords).
    pub offsets: Vec<usize>,
}

impl ratatui::widgets::WidgetRef for LogWidget {
    fn render_ref(&self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        use ratatui::style::{Color, Modifier, Style};

        let gray = Style::default().fg(Color::DarkGray);

        // Selection in ordered form (top-left to bottom-right)
        let sel = self.selection.map(|(sr, sc, er, ec)| {
            if (sr, sc) > (er, ec) { (er, ec, sr, sc) } else { (sr, sc, er, ec) }
        });

        for (row, line) in self.lines.iter().enumerate() {
            if row >= area.height as usize { break; }
            let content_row = self.viewport_start + row;
            let is_cont = line.starts_with("↪ ");
            let y = area.y + row as u16;
            let chars: Vec<char> = line.chars().collect();

            for (col, ch) in chars.iter().enumerate() {
                if col >= area.width as usize { break; }
                let x = area.x + col as u16;

                // Check selection using content_row directly (no viewport translation needed)
                let is_selected = match sel {
                    Some((sr, sc, er, ec)) if self.rect => {
                        // Content-based rectangle: highlight chars whose
                        // ORIGINAL-line position is in [cmin, cmax). The "↪ "
                        // indent is a marker, never part of the content.
                        let (cmin, cmax) = (sc.min(ec), sc.max(ec));
                        if content_row >= sr && content_row <= er {
                            let indent = row_content(line).1;
                            if col >= indent {
                                let o = self.offsets.get(content_row).copied().unwrap_or(0)
                                    + (col - indent);
                                o >= cmin && o < cmax
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    }
                    Some((sr, sc, er, ec)) => {
                        if content_row > sr && content_row < er { true }
                        else if content_row == sr && content_row == er { col >= sc && col < ec }
                        else if content_row == sr { col >= sc }
                        else if content_row == er { col < ec }
                        else { false }
                    }
                    None => false,
                };

                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(*ch);
                    if is_selected {
                        cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
                    } else if is_cont && col < 2 {
                        cell.set_style(gray);
                    }
                }
            }
        }
    }
}

/// Run the TUI client without an overlay callback.
///
/// # Deprecated
///
/// To be removed after **October 26**. Use [`run_tui_with_events`] — or the
/// `servatui-display` crate's `Display::run`, which builds on it — instead.
#[deprecated(
    since = "0.7.1",
    note = "replaced by run_tui_with_events (or the servatui-display crate); removal scheduled after October 26"
)]
#[cfg(feature = "tui")]
pub fn run_tui(socket: &Path, protocols: &[Protocol]) -> Result<(), String> {
    run_tui_with_overlay(socket, protocols, |_| {})
}

/// Run the TUI client with an overlay callback.
#[cfg(feature = "tui")]
pub fn run_tui_with_overlay<F>(
    socket: &Path,
    protocols: &[Protocol],
    on_overlay: F,
) -> Result<(), String>
where
    F: FnMut(&mut Vec<WidgetEntry>),
{
    run_tui_with_events(socket, protocols, on_overlay, |_| false)
}

/// Run the TUI client with an overlay callback and an event hook.
///
/// The event hook is consulted for every terminal event BEFORE the builtin
/// handling (input line, log selection, scrollbar). Returning `true`
/// swallows the event: the builtin handling is skipped entirely. This is
/// the integration point for event-routing layers on top of servatui
/// (see the `servatui-display` crate); unhandled events fall through so a
/// plain embedder behaves exactly like [`run_tui_with_overlay`].
#[cfg(feature = "tui")]
pub fn run_tui_with_events<F, G>(
    socket: &Path,
    protocols: &[Protocol],
    mut on_overlay: F,
    mut on_event: G,
) -> Result<(), String>
where
    F: FnMut(&mut Vec<WidgetEntry>),
    G: FnMut(&crossterm::event::Event) -> bool,
{
    use crossterm::execute;
    use crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
    use crossterm::event::EnableMouseCapture;
    use ratatui::{backend::CrosstermBackend, Terminal};

    // Restore on every abnormal path (signals, panics) — not just the
    // normal-exit one below. Installed before the modes are enabled so
    // a failure halfway through setup is also covered.
    let _guard = crate::terminal_restore::TerminalGuard::install();

    enable_raw_mode().map_err(|e| e.to_string())?;
    execute!(std::io::stdout(), EnterAlternateScreen, EnableMouseCapture).map_err(|e| e.to_string())?;
    std::io::stdout().flush().ok();

    let backend = SizeOverrideBackend { inner: CrosstermBackend::new(std::io::stdout()) };
    let mut terminal = Terminal::new(backend).map_err(|e| e.to_string())?;

    let mut state = TuiState::new();
    let mut input = Input::default();
    let mut mouse = MouseState::default();

    let result = tui_loop(
        &mut terminal, &mut state, &mut input,
        socket, protocols,
        &mut TuiHooks { on_overlay: &mut on_overlay, on_event: &mut on_event },
        &mut mouse,
    );

    // The guard's Drop also restores (idempotent) — this explicit call
    // just ensures the farewell lands on the main screen.
    crate::terminal_restore::restore_terminal();
    println!("Goodbye.");
    result
}

/// Cached viewport info for mouse coordinate translation.
///
/// Public (with public fields) for the fuzz harnesses; not stable API.
pub struct ViewportCache {
    pub log_area: ratatui::layout::Rect,
    pub log_inner: ratatui::layout::Rect,
    pub viewport_start: usize,
    pub total_wrapped: usize,
    pub wrapped: Vec<String>,
    /// Per wrapped row: char offset into its original line (content coords).
    pub offsets: Vec<usize>,
}

/// The two embedder hooks `tui_loop` consults: the per-frame overlay
/// callback and the per-event swallow callback.
struct TuiHooks<'a> {
    on_overlay: &'a mut dyn FnMut(&mut Vec<WidgetEntry>),
    on_event: &'a mut dyn FnMut(&crossterm::event::Event) -> bool,
}

#[cfg(feature = "tui")]
fn tui_loop<B>(
    terminal: &mut ratatui::Terminal<B>,
    state: &mut TuiState,
    input: &mut tui_input::Input,
    socket: &Path,
    protocols: &[Protocol],
    hooks: &mut TuiHooks<'_>,
    mouse: &mut MouseState,
) -> Result<(), String>
where
    B: ratatui::backend::Backend,
{
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    use ratatui::{
        layout::{Constraint, Direction, Layout},
        widgets::{Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
    };
    use tui_input::backend::crossterm::EventHandler;

    let mut vp: Option<ViewportCache> = None;
    let mut completion = CompletionState::default();
    let mut blink_on = true;
    let mut last_blink = Instant::now();

    loop {
        // Slow flash of the unconfirmed suggestion tail (~600ms phase).
        if last_blink.elapsed() >= Duration::from_millis(600) {
            last_blink = Instant::now();
            blink_on = !blink_on;
        }
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(3)])
                .split(f.area());

            let log_area = chunks[0];
            let log_inner = log_area.inner(ratatui::layout::Margin { horizontal: 1, vertical: 1 });
            let log_height = log_inner.height as usize;
            let inner_width = log_inner.width as usize;

            // Pre-wrap log lines, remembering each row's offset into its
            // original line (content coordinates for rectangular selection).
            let mut wrapped: Vec<String> = Vec::with_capacity(state.log_lines.len());
            let mut offsets: Vec<usize> = Vec::with_capacity(state.log_lines.len());
            for s in &state.log_lines {
                let clean = s.rsplit_once('\r').map(|(_, last)| last).unwrap_or(s);
                let clean = clean.replace('\t', "    ");
                for (row, off) in wrap_with_offsets(&clean, inner_width, "↪ ") {
                    wrapped.push(row);
                    offsets.push(off);
                }
            }

            let total_rows = wrapped.len();
            let max_scroll = total_rows.saturating_sub(log_height);
            state.scroll_up = (state.scroll_up as usize).min(max_scroll) as u16;

            let title = if state.scroll_up > 0 {
                format!("Log (\u{2191}{} rows)", state.scroll_up)
            } else {
                "Log".to_string()
            };

            let start = total_rows.saturating_sub(log_height).saturating_sub(state.scroll_up as usize);
            let visible: Vec<String> = if total_rows > 0 { wrapped[start..].to_vec() } else { vec![] };

            vp = Some(ViewportCache {
                log_area,
                log_inner,
                viewport_start: start,
                total_wrapped: total_rows,
                wrapped: wrapped.clone(),
                offsets: offsets.clone(),
            });

            f.render_widget(Clear, log_area);
            f.render_widget(Block::default().borders(Borders::ALL).title(title), log_area);

            let mut widgets: Vec<WidgetEntry> = Vec::new();

            widgets.push(WidgetEntry {
                name: WIDGET_LOG,
                widget: Box::new(LogWidget {
                    lines: visible,
                    selection: state.selection,
                    viewport_start: start,
                    rect: state.selection_rect,
                    offsets: offsets.clone(),
                }),
                area: log_inner,
            });

            let prompt = "> ";
            let input_area = chunks[1];
            let (confirmed_part, ghost) = split_confirmed(input.value(), completion.confirmed);
            let input_line = if ghost.is_empty() {
                ratatui::text::Line::from(format!("{prompt}{confirmed_part}"))
            } else {
                // The unconfirmed tail flashes: alternating between hidden and
                // emphasized, toggled by the blink driver above.
                let ghost_style = if blink_on {
                    ratatui::style::Style::new().add_modifier(ratatui::style::Modifier::HIDDEN)
                } else {
                    ratatui::style::Style::new().add_modifier(ratatui::style::Modifier::REVERSED)
                };
                ratatui::text::Line::from(vec![
                    ratatui::text::Span::raw(format!("{prompt}{confirmed_part}")),
                    ratatui::text::Span::styled(ghost.to_string(), ghost_style),
                ])
            };
            widgets.push(WidgetEntry {
                name: WIDGET_INPUT,
                widget: Box::new(
                    Paragraph::new(input_line)
                        .block(Block::default().borders(Borders::ALL).title("Input")),
                ),
                area: input_area,
            });

            (hooks.on_overlay)(&mut widgets);

            for entry in &widgets {
                entry.widget.render_ref(entry.area, f.buffer_mut());
            }

            // Scrollbar
            let sb_scroll = max_scroll.saturating_sub(state.scroll_up as usize);
            let mut sb_state = ScrollbarState::new(max_scroll + 1)
                .position(sb_scroll)
                .viewport_content_length(log_height);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None).end_symbol(None),
                log_area.inner(ratatui::layout::Margin { horizontal: 0, vertical: 1 }),
                &mut sb_state,
            );

            f.set_cursor_position((
                input_area.x + 1 + prompt.len() as u16 + input.visual_cursor() as u16,
                input_area.y + 1,
            ));
        }).map_err(|e| e.to_string())?;

        // Use shorter poll when auto-scrolling for smooth UX
        let poll_ms = if mouse.auto_scroll.is_some() { 30 } else { 100 };

        if event::poll(Duration::from_millis(poll_ms)).map_err(|e| e.to_string())? {
            let ev = event::read().map_err(|e| e.to_string())?;

            // Event hook: swallowing an event skips the builtin handling.
            if (hooks.on_event)(&ev) {
                continue;
            }

            match ev {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    // Any key other than Tab confirms the whole input line
                    // (including a flashing, unconfirmed suggestion tail).
                    // Esc is the other exception: it DELETES the tail instead
                    // of confirming it, so it must not pass the gate first.
                    if key.code != KeyCode::Tab && key.code != KeyCode::Esc {
                        confirm_all(input, &mut completion);
                    }
                    match key.code {
                        KeyCode::PageUp => { state.scroll_up = state.scroll_up.saturating_add(5); continue; }
                        KeyCode::PageDown => { state.scroll_up = state.scroll_up.saturating_sub(5); continue; }
                        KeyCode::Home => { state.scroll_up = u16::MAX; continue; }
                        KeyCode::End => { state.scroll_up = 0; continue; }
                        KeyCode::Up if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                            state.scroll_up = state.scroll_up.saturating_add(1); continue;
                        }
                        KeyCode::Down if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                            state.scroll_up = state.scroll_up.saturating_sub(1); continue;
                        }
                        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) && input.value().is_empty() => {
                            return Ok(());
                        }
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            input.reset(); state.history_idx = None;
                            confirm_all(input, &mut completion);
                            continue;
                        }
                        KeyCode::Esc => {
                            esc_unconfirmed(input, &mut completion);
                            continue;
                        }
                        KeyCode::Tab => {
                            tab_complete(input, &mut completion, protocols);
                            continue;
                        }
                        KeyCode::Enter => {
                            let line = input.value().to_string();
                            input.reset();
                            confirm_all(input, &mut completion);
                            state.history_idx = None;
                            let trimmed = line.trim();
                            if trimmed.is_empty() { continue; }
                            if trimmed == "exit" || trimmed == "quit" { return Ok(()); }
                            if trimmed == "help" {
                                state.log_lines.push("> help".into());
                                state.log_lines.push("COMMANDS:".into());
                                for p in protocols {
                                    state.log_lines.push(format!("  {:<12} {}", p.name, p.help));
                                }
                                state.scroll_up = 0;
                                state.selection = None; // Clear on new output
                                continue;
                            }

                            state.history.push(line.clone());
                            state.log_lines.push(format!("> {trimmed}"));
                            state.scroll_up = 0;
                            state.selection = None; // Clear on new output

                            let (cmd_name, args) = match trimmed.split_once(' ') {
                                Some((n, r)) => (n, r),
                                None => (trimmed, ""),
                            };

                            let proto = match protocols.iter().find(|p| p.name == cmd_name) {
                                Some(p) => p,
                                None => {
                                    state.log_lines.push(format!("Unknown: '{cmd_name}'"));
                                    continue;
                                }
                            };

                            match execute_command(proto, args, socket) {
                                Ok(output_lines) => state.log_lines.extend(output_lines),
                                Err(e) => state.log_lines.push(format!("Error: {e}")),
                            }
                        }
                        KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            if !state.history.is_empty() {
                                state.history_idx = Some(match state.history_idx {
                                    Some(0) => 0,
                                    Some(i) => i - 1,
                                    None => state.history.len() - 1,
                                });
                                *input = tui_input::Input::new(state.history[state.history_idx.unwrap()].clone());
                                confirm_all(input, &mut completion);
                            }
                        }
                        KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            match state.history_idx {
                                Some(i) if i + 1 < state.history.len() => {
                                    let new_idx = i + 1;
                                    state.history_idx = Some(new_idx);
                                    *input = tui_input::Input::new(state.history[new_idx].clone());
                                    confirm_all(input, &mut completion);
                                }
                                _ => { state.history_idx = None; input.reset(); }
                            }
                        }
                        _ => {
                            let _ = input.handle_event(&Event::Key(key));
                            // The just-inserted edit is confirmed too: the
                            // boundary must never lag the last typed char.
                            confirm_all(input, &mut completion);
                        }
                    }
                }
                Event::Mouse(m) => {
                    if let Some(ref vp) = vp {
                        handle_mouse_event(m, state, mouse, vp);
                    }
                }
                _ => {}
            }
        } else {
            // Poll timeout: handle auto-scroll during drag
            if let (Some(ref vp), Some(scroll_up_dir)) = (&vp, mouse.auto_scroll) {
                if mouse.selecting {
                    let max_scroll = vp.total_wrapped.saturating_sub(vp.log_inner.height as usize);
                    if scroll_up_dir {
                        // Auto-scroll up (towards older content)
                        if (state.scroll_up as usize) < max_scroll {
                            state.scroll_up += 1;
                        }
                        // Extend selection to top of viewport
                        let new_start = vp.total_wrapped.saturating_sub(vp.log_inner.height as usize)
                            .saturating_sub(state.scroll_up as usize);
                        if let Some(anchor) = mouse.anchor {
                            update_selection(state, mouse, anchor, (new_start, 0), &vp.wrapped, &vp.offsets);
                        }
                    } else {
                        // Auto-scroll down (towards newer content)
                        if state.scroll_up > 0 {
                            state.scroll_up -= 1;
                        }
                        // Extend selection to bottom of viewport
                        let new_start = vp.total_wrapped.saturating_sub(vp.log_inner.height as usize)
                            .saturating_sub(state.scroll_up as usize);
                        let bottom_row = new_start + vp.log_inner.height as usize;
                        if let Some(anchor) = mouse.anchor {
                            update_selection(state, mouse, anchor, (bottom_row, usize::MAX), &vp.wrapped, &vp.offsets);
                        }
                    }
                }
            }
        }
    }
}

/// Input-shell completion state: the input line is split into a CONFIRMED
/// prefix and an unconfirmed tail (e.g. a tab-completion suggestion).
/// Everything past `confirmed` flashes; ESC deletes it; any key other than
/// Tab confirms it; Tab cycles suggestions without confirming them.
#[derive(Default)]
pub(crate) struct CompletionState {
    /// Number of CHARS of `input` that are confirmed.
    pub confirmed: usize,
    /// Index of the currently shown suggestion (None = none shown yet).
    pub index: Option<usize>,
}

/// Split `value` at the `confirmed`-th char (char-boundary safe).
fn split_confirmed(value: &str, confirmed: usize) -> (&str, &str) {
    let byte = value
        .char_indices()
        .nth(confirmed)
        .map(|(b, _)| b)
        .unwrap_or(value.len());
    value.split_at(byte)
}

/// Confirm the whole input line (any key except Tab does this first).
fn confirm_all(input: &mut Input, comp: &mut CompletionState) {
    comp.confirmed = input.value().chars().count();
    comp.index = None;
}

/// ESC: delete the unconfirmed tail, reset the suggestion cycle.
fn esc_unconfirmed(input: &mut Input, comp: &mut CompletionState) {
    let confirmed: String = input.value().chars().take(comp.confirmed).collect();
    *input = Input::new(confirmed);
    comp.confirmed = input.value().chars().count();
    comp.index = None;
}

/// Suggestions for the confirmed string: command names while the first word
/// is still being typed; the plugin's completer once arguments have started.
fn suggestions_for(protocols: &[Protocol], confirmed: &str) -> Vec<String> {
    let first = confirmed.split_whitespace().next().unwrap_or("");
    if confirmed.chars().any(char::is_whitespace) {
        protocols
            .iter()
            .find(|p| p.name == first)
            .and_then(|p| p.completer.as_ref())
            .map(|c| c(confirmed))
            .unwrap_or_default()
    } else if first.is_empty() {
        Vec::new()
    } else {
        protocols
            .iter()
            .filter(|p| p.name.starts_with(first))
            .map(|p| p.name.to_string())
            .collect()
    }
}

/// Tab: apply the next suggestion as an UNCONFIRMED tail (flashing).
/// The completer only ever sees the confirmed string, so repeated Tabs cycle
/// through suggestions computed from the same base.
fn tab_complete(input: &mut Input, comp: &mut CompletionState, protocols: &[Protocol]) {
    let confirmed: String = input.value().chars().take(comp.confirmed).collect();
    let options = suggestions_for(protocols, &confirmed);
    let n = options.len();
    if n == 0 {
        return;
    }
    let idx = comp.index.map(|i| (i + 1) % n).unwrap_or(0);
    comp.index = Some(idx);
    let suggestion = options[idx].clone();
    *input = Input::new(suggestion).with_cursor(comp.confirmed);
}

/// Update selection based on anchor + current position.
/// The selection always includes BOTH the anchor character and the character
/// under the current position.
fn update_selection(
    state: &mut TuiState,
    mouse: &mut MouseState,
    anchor: (usize, usize),
    current: (usize, usize),
    wrapped: &[String],
    offsets: &[usize],
) {
    // Clamp current col to line width
    let cur_row = current.0.min(wrapped.len().saturating_sub(1));
    // Display length (includes the "↪ " indent): the drag column is a display
    // column, so clamping to the content-only length would clip the last char(s)
    // of a continuation row.
    let line_len = if cur_row < wrapped.len() {
        wrapped[cur_row].chars().count()
    } else { 0 };
    let cur_col = current.1.min(line_len);

    match mouse.click_mode {
        ClickMode::Char => {
            // Both anchor and current positions should be included.
            // Use inclusive start + exclusive end, adjusting based on direction.
            if state.selection_rect {
                // Rectangular selection stores ORIGINAL-line columns: the box's
                // x-range must mean the same content position on every row.
                let a_col = orig_col(wrapped, offsets, anchor.0, anchor.1);
                let c_col = orig_col(wrapped, offsets, cur_row, cur_col);
                if (anchor.0, a_col) <= (cur_row, c_col) {
                    state.selection = Some((anchor.0, a_col, cur_row, c_col + 1));
                } else {
                    state.selection = Some((cur_row, c_col, anchor.0, a_col + 1));
                }
            } else if (anchor.0, anchor.1) <= (cur_row, cur_col) {
                // Dragging right/down: anchor is start (inclusive), current is end (inclusive → +1)
                state.selection = Some((anchor.0, anchor.1, cur_row, cur_col + 1));
            } else {
                // Dragging left/up: current is start (inclusive), anchor is end (inclusive → +1)
                state.selection = Some((cur_row, cur_col, anchor.0, anchor.1 + 1));
            }
        }
        ClickMode::Word => {
            if cur_row < wrapped.len() {
                let (_, we) = word_boundaries(&wrapped[cur_row], cur_col);
                state.selection = Some((anchor.0, anchor.1, cur_row, we));
            }
        }
        ClickMode::Line => {
            state.selection = Some((anchor.0, 0, cur_row, line_len));
        }
    }
}

/// Process one mouse event against the TUI/selection state.
///
/// Public for the fuzz harnesses; not stable API.
pub fn handle_mouse_event(
    m: crossterm::event::MouseEvent,
    state: &mut TuiState,
    mouse: &mut MouseState,
    vp: &ViewportCache,
) {
    use crossterm::event::{KeyModifiers, MouseEventKind, MouseButton};

    let inner = vp.log_inner;
    let area = vp.log_area;

    // Check position zones
    let in_log = m.column >= inner.x
        && m.column < inner.x + inner.width
        && m.row >= inner.y
        && m.row < inner.y + inner.height;

    let scrollbar_col = area.x + area.width - 1;
    let on_scrollbar = m.column == scrollbar_col
        && m.row > area.y
        && m.row < area.y + area.height - 1;

    // Translate screen to content coordinates
    let content_row = |mouse_row: u16| -> usize {
        let vp_row = mouse_row.saturating_sub(inner.y) as usize;
        vp.viewport_start + vp_row
    };
    let content_col = |mouse_col: u16| -> usize {
        mouse_col.saturating_sub(inner.x) as usize
    };

    match m.kind {
        MouseEventKind::ScrollUp if in_log || on_scrollbar => {
            state.scroll_up = state.scroll_up.saturating_add(3);
        }
        MouseEventKind::ScrollDown if in_log || on_scrollbar => {
            state.scroll_up = state.scroll_up.saturating_sub(3);
        }

        MouseEventKind::Down(MouseButton::Left) => {
            if on_scrollbar {
                mouse.scrollbar_drag = true;
                // Fix: top of scrollbar = top of content = high scroll_up
                let track_h = (area.height.saturating_sub(2)) as usize;
                let click_y = (m.row - area.y - 1) as usize;
                let max_scroll = vp.total_wrapped.saturating_sub(inner.height as usize);
                state.scroll_up = max_scroll.saturating_sub(
                    click_y * max_scroll / track_h.max(1)
                ) as u16;
                return;
            }

            if !in_log {
                mouse.selecting = false;
                mouse.auto_scroll = None;
                state.selection = None;
                return;
            }

            // Click counting for double/triple click
            let now = Instant::now();
            let same_pos = mouse.last_click_pos == (m.column, m.row);
            let fast = now.duration_since(mouse.last_click_time) < Duration::from_millis(500);

            if same_pos && fast {
                mouse.click_count += 1;
            } else {
                mouse.click_count = 1;
            }
            mouse.last_click_time = now;
            mouse.last_click_pos = (m.column, m.row);

            mouse.click_mode = match mouse.click_count {
                1 => ClickMode::Char,
                2 => ClickMode::Word,
                _ => ClickMode::Line,
            };

            mouse.selecting = true;
            mouse.auto_scroll = None;

            let crow = content_row(m.row);
            let ccol = content_col(m.column);

            // Set anchor
            // Rectangular (column) selection is Alt+drag, like VS Code. Decided
            // at mouse-down from the event modifiers (not a sticky flag), so it
            // can't get stuck on after a stray key press.
            let alt = m.modifiers.contains(KeyModifiers::ALT);
            match mouse.click_mode {
                ClickMode::Char => {
                    mouse.anchor = Some((crow, ccol));
                    state.selection_rect = alt;
                    // Rectangular selection anchors in ORIGINAL-line columns.
                    let col = if alt {
                        orig_col(&vp.wrapped, &vp.offsets, crow, ccol)
                    } else {
                        ccol
                    };
                    state.selection = Some((crow, col, crow, col + 1));
                }
                ClickMode::Word => {
                    state.selection_rect = false;
                    if crow < vp.wrapped.len() {
                        // Pass the full wrapped row (with "↪ " indent): the column
                        // is a display column, so the word boundaries must be too.
                        let (ws, we) = word_boundaries(&vp.wrapped[crow], ccol);
                        mouse.anchor = Some((crow, ws));
                        state.selection = Some((crow, ws, crow, we));
                    }
                }
                ClickMode::Line => {
                    state.selection_rect = false;
                    let line_len = if crow < vp.wrapped.len() {
                        vp.wrapped[crow].chars().count()
                    } else { 0 };
                    mouse.anchor = Some((crow, 0));
                    state.selection = Some((crow, 0, crow, line_len));
                }
            }
        }

        MouseEventKind::Drag(MouseButton::Left) => {
            if mouse.scrollbar_drag {
                if on_scrollbar || m.column == scrollbar_col {
                    let track_h = (area.height.saturating_sub(2)) as usize;
                    // Clamp at the top of the track: the drag may report a
                    // row on (or above) the top border row.
                    let click_y = m.row.saturating_sub(area.y + 1) as usize;
                    let max_scroll = vp.total_wrapped.saturating_sub(inner.height as usize);
                    state.scroll_up = max_scroll.saturating_sub(
                        click_y * max_scroll / track_h.max(1)
                    ) as u16;
                }
                return;
            }

            if !mouse.selecting {
                return;
            }

            // Check for auto-scroll: mouse above or below viewport
            if m.row < inner.y {
                // Auto-scroll up (toward older content) immediately
                let max_scroll = vp.total_wrapped.saturating_sub(inner.height as usize);
                if (state.scroll_up as usize) < max_scroll {
                    state.scroll_up += 1;
                }
                let new_start = vp.total_wrapped.saturating_sub(inner.height as usize)
                    .saturating_sub(state.scroll_up as usize);
                if let Some(anchor) = mouse.anchor {
                    update_selection(state, mouse, anchor, (new_start, 0), &vp.wrapped, &vp.offsets);
                }
                mouse.auto_scroll = Some(true);
                return;
            } else if m.row >= inner.y + inner.height {
                // Auto-scroll down (toward newer content) immediately
                if state.scroll_up > 0 {
                    state.scroll_up -= 1;
                }
                let new_start = vp.total_wrapped.saturating_sub(inner.height as usize)
                    .saturating_sub(state.scroll_up as usize);
                let bottom_row = new_start + inner.height as usize;
                if let Some(anchor) = mouse.anchor {
                    update_selection(state, mouse, anchor, (bottom_row, usize::MAX), &vp.wrapped, &vp.offsets);
                }
                mouse.auto_scroll = Some(false);
                return;
            } else {
                mouse.auto_scroll = None;
            }

            // Normal drag within viewport
            let crow = content_row(m.row);
            let ccol = content_col(m.column);
            if let Some(anchor) = mouse.anchor {
                update_selection(state, mouse, anchor, (crow, ccol), &vp.wrapped, &vp.offsets);
            }
        }

        MouseEventKind::Up(MouseButton::Left) => {
            if mouse.scrollbar_drag {
                mouse.scrollbar_drag = false;
                return;
            }

            if mouse.selecting {
                mouse.selecting = false;
                mouse.auto_scroll = None;

                // Copy selection to clipboard
                if let Some((sr, sc, er, ec)) = state.selection {
                    let text = extract_selection(&vp.wrapped, &vp.offsets, sr, sc, er, ec, state.selection_rect);
                    if !text.is_empty() {
                        osc52_copy(&text);
                    }
                }
            }
        }

        MouseEventKind::Down(MouseButton::Right) => {
            // Right click: extend selection to clicked position
            if in_log && state.selection.is_some() {
                let crow = content_row(m.row);
                let ccol = content_col(m.column);
                if let Some(anchor) = mouse.anchor {
                    update_selection(state, mouse, anchor, (crow, ccol), &vp.wrapped, &vp.offsets);
                    // Copy on extend
                    if let Some((sr, sc, er, ec)) = state.selection {
                        let text = extract_selection(&vp.wrapped, &vp.offsets, sr, sc, er, ec, state.selection_rect);
                        if !text.is_empty() { osc52_copy(&text); }
                    }
                }
            } else {
                state.selection = None;
            }
        }

        MouseEventKind::Down(MouseButton::Middle) => {
            // Middle click: paste from clipboard
            osc52_paste();
        }

        _ => {}
    }
}

fn execute_command(proto: &Protocol, args: &str, socket: &Path) -> Result<Vec<String>, String> {
    let mut console = TuiBufferConsole::new();
    let mut input_src = crate::console::NoInput;

    let mut conn = match SocketConnection::connect(socket) {
        Ok(c) => c,
        Err(e) => {
            if let Some(offline) = &proto.offline {
                offline(args, &mut console)?;
                return Ok(console.lines);
            }
            return Err(e);
        }
    };
    conn.send_typed(&proto.name.to_string())?;
    proto.run_client(args, &mut conn, &mut console, &mut input_src)?;
    Ok(console.lines)
}

struct TuiBufferConsole {
    lines: Vec<String>,
}

impl TuiBufferConsole {
    fn new() -> Self { Self { lines: Vec::new() } }
}

impl Console for TuiBufferConsole {
    fn print_line(&mut self, text: &str) {
        self.lines.push(text.to_string());
    }
    fn print_error(&mut self, text: &str) {
        self.lines.push(format!("Error: {text}"));
    }
}

// ═══════════════════════════════════════════════════════════════
// Tests for mouse selection behavior
// ═══════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn completion_protocol(name: &'static str) -> Protocol {
        crate::Plugin::new(name, "test cmd")
            .parse(|_: &str| Ok(()))
            .client(|_: (), _: &mut dyn crate::Console, _: &mut dyn crate::InputSource| Ok(()))
            .server(|_: ()| Ok(()))
            .client(|_: (), _: &mut dyn crate::Console, _: &mut dyn crate::InputSource| Ok(()))
            .finalize(|| Ok(crate::ShellAction::Continue))
    }

    // ── tab completion: ghost + confirm model ─────────────────────

    #[test]
    fn tab_completes_command_names_as_ghost() {
        let protocols = [completion_protocol("lorem"), completion_protocol("status")];
        let mut input = tui_input::Input::new("lo".into());
        let mut comp = CompletionState { confirmed: 2, ..Default::default() };
        tab_complete(&mut input, &mut comp, &protocols);
        assert_eq!(input.value(), "lorem");
        assert_eq!(comp.confirmed, 2, "a suggestion must NOT be confirmed");
        assert_eq!(input.cursor(), 2, "cursor stays at the confirmed boundary");
    }

    #[test]
    fn tab_cycles_through_suggestions() {
        let protocols = [
            completion_protocol("lorem"),
            completion_protocol("logout"),
            completion_protocol("low"),
        ];
        let mut input = tui_input::Input::new("lo".into());
        let mut comp = CompletionState { confirmed: 2, ..Default::default() };
        for expected in ["lorem", "logout", "low", "lorem"] {
            tab_complete(&mut input, &mut comp, &protocols);
            assert_eq!(input.value(), expected);
            assert_eq!(comp.confirmed, 2);
        }
    }

    #[test]
    fn plugin_completer_receives_confirmed_string_only() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_c = seen.clone();
        let protocols = [completion_protocol("lorem").complete(move |s: &str| {
            seen_c.lock().unwrap().push(s.to_string());
            vec![format!("{s}alpha"), format!("{s}beta")]
        })];
        let mut input = tui_input::Input::new("lorem a".into());
        let mut comp = CompletionState { confirmed: 7, ..Default::default() };

        tab_complete(&mut input, &mut comp, &protocols);
        assert_eq!(input.value(), "lorem aalpha", "first suggestion applied");
        assert_eq!(comp.confirmed, 7, "applied suggestion stays unconfirmed");

        tab_complete(&mut input, &mut comp, &protocols);
        assert_eq!(input.value(), "lorem abeta", "second suggestion cycled in");
        // The completer must have seen the CONFIRMED string both times —
        // never the ghost text from the first suggestion.
        assert_eq!(*seen.lock().unwrap(), vec!["lorem a", "lorem a"]);
    }

    #[test]
    fn esc_trims_to_confirmed() {
        let protocols = [completion_protocol("lorem").complete(|s: &str| vec![format!("{s}xyz")])];
        let mut input = tui_input::Input::new("lorem a".into());
        let mut comp = CompletionState { confirmed: 7, ..Default::default() };
        tab_complete(&mut input, &mut comp, &protocols);
        assert_eq!(input.value(), "lorem axyz");

        esc_unconfirmed(&mut input, &mut comp);
        assert_eq!(input.value(), "lorem a", "ESC deletes the unconfirmed part");
        assert_eq!(comp.confirmed, 7);
        assert_eq!(comp.index, None, "cycle resets after ESC");
    }

    #[test]
    fn any_key_confirms_everything() {
        let protocols = [completion_protocol("lorem").complete(|s: &str| vec![format!("{s}xyz")])];
        let mut input = tui_input::Input::new("lorem a".into());
        let mut comp = CompletionState { confirmed: 7, ..Default::default() };
        tab_complete(&mut input, &mut comp, &protocols);
        assert!(input.value().len() > comp.confirmed);

        confirm_all(&mut input, &mut comp);
        assert_eq!(comp.confirmed, input.value().chars().count());
        assert_eq!(comp.index, None, "cycle resets on confirm");

        // A subsequent tab re-completes from the now-longer confirmed base.
        tab_complete(&mut input, &mut comp, &protocols);
        assert_eq!(input.value(), "lorem axyzxyz");
    }

    #[test]
    fn split_confirmed_is_char_safe() {
        let (a, b) = split_confirmed("aé你bc", 3);
        assert_eq!((a, b), ("aé你", "bc"));
        let (a, b) = split_confirmed("aé你bc", 5);
        assert_eq!((a, b), ("aé你bc", ""));
        let (a, b) = split_confirmed("aé你bc", 0);
        assert_eq!((a, b), ("", "aé你bc"));
    }

    #[test]
    fn tab_without_matches_is_noop() {
        let protocols = [completion_protocol("lorem")];
        let mut input = tui_input::Input::new("zz".into());
        let mut comp = CompletionState { confirmed: 2, ..Default::default() };
        tab_complete(&mut input, &mut comp, &protocols);
        assert_eq!(input.value(), "zz");
        assert_eq!(comp.index, None);
    }

    #[test]
    fn args_position_without_completer_is_noop() {
        let protocols = [completion_protocol("lorem")];
        let mut input = tui_input::Input::new("lorem x".into());
        let mut comp = CompletionState { confirmed: 8, ..Default::default() };
        tab_complete(&mut input, &mut comp, &protocols);
        assert_eq!(input.value(), "lorem x");
        assert_eq!(comp.index, None);
    }

    #[test]
    fn completer_defaults_to_none() {
        assert!(completion_protocol("x").completer.is_none());
    }

    fn make_mouse_state() -> MouseState {
        MouseState::default()
    }

    fn make_state(lines: Vec<&str>) -> TuiState {
        TuiState {
            log_lines: lines.into_iter().map(String::from).collect(),
            ..TuiState::new()
        }
    }

    // ── Bug 3 test: leftward drag must include char under mouse ──────

    #[test]
    fn test_leftward_drag_includes_anchor_char() {
        // Drag from col 5 to col 2 on the same row.
        // The selection should include cols 2..=5 (inclusive both ends).
        let mut state = make_state(vec!["ABCDEFGH"]);
        let mut mouse = make_mouse_state();
        mouse.selecting = true;
        mouse.click_mode = ClickMode::Char;
        mouse.anchor = Some((0, 5));

        // Drag to col 2 (leftward)
        let wrapped = state.log_lines.clone();
        update_selection(&mut state, &mut mouse, (0, 5), (0, 2), &wrapped, &[]);

        let (sr, sc, er, ec) = state.selection.unwrap();
        // After ordering: start should be (0, 2), end should be (0, 6)
        // So the selection covers cols 2,3,4,5 — both the anchor (5) and current (2)
        assert_eq!((sr, sc), (0, 2), "leftward: start should be at current position");
        assert_eq!((er, ec), (0, 6), "leftward: end should be anchor+1 (inclusive)");
    }

    #[test]
    fn test_rightward_drag_includes_current_char() {
        // Drag from col 2 to col 5 on the same row.
        let mut state = make_state(vec!["ABCDEFGH"]);
        let mut mouse = make_mouse_state();
        mouse.selecting = true;
        mouse.click_mode = ClickMode::Char;
        mouse.anchor = Some((0, 2));

        let wrapped = state.log_lines.clone();
        update_selection(&mut state, &mut mouse, (0, 2), (0, 5), &wrapped, &[]);

        let (sr, sc, er, ec) = state.selection.unwrap();
        assert_eq!((sr, sc), (0, 2), "rightward: start should be at anchor");
        assert_eq!((er, ec), (0, 6), "rightward: end should be current+1 (inclusive)");
    }

    #[test]
    fn test_extract_leftward_drag_text() {
        // Verify the extracted text includes both endpoints
        let wrapped = vec!["ABCDEFGH".to_string()];
        // Selection: cols 2..6 (CDEF)
        let text = extract_selection(&wrapped, &[], 0, 2, 0, 6, false);
        assert_eq!(text, "CDEF", "should include both endpoints");
    }

    #[test]
    fn test_extract_continuation_row_not_shifted_by_indent() {
        // Logical line "ABCDEFGH" wrapped into two display rows:
        //   row 0 "ABCD"      (no indent)
        //   row 1 "↪ EFGH"    (continuation; content "EFGH" at display cols 2..=5)
        // Selection columns are display coords (they include the "↪ " indent), so
        // "EFGH" is cols 2..6 on row 1. Extraction must NOT be shifted by the
        // indent and must yield "EFGH".
        let wrapped = vec!["ABCD".to_string(), "↪ EFGH".to_string()];
        assert_eq!(extract_selection(&wrapped, &[0], 1, 2, 1, 6, false), "EFGH");
    }

    #[test]
    fn test_extract_across_wrap_keeps_break_space() {
        // "hello world" wrapped at the space → ["hello", "↪ world"].
        // Copying the whole line must reinsert the space consumed at the wrap.
        let wrapped = vec!["hello".to_string(), "↪ world".to_string()];
        let end = "↪ world".chars().count(); // display length of the last row
        assert_eq!(extract_selection(&wrapped, &[0, 6], 0, 0, 1, end, false), "hello world");
    }

    #[test]
    fn test_extract_partial_across_wrap_keeps_break_space() {
        // Select from the middle of the first row into its continuation row.
        let wrapped = vec!["he-llo".to_string(), "↪ world".to_string()];
        let end = "↪ world".chars().count();
        // cols 3..end on row 0, then all of the continuation → "llo world"
        assert_eq!(extract_selection(&wrapped, &[0, 3], 0, 3, 1, end, false), "llo world");
    }

    #[test]
    fn test_select_through_last_char_of_continuation_row() {
        // Continuation row "↪ def": content "def" is at display cols 2,3,4.
        // Drag from 'd' (col 2) to 'f' (col 4) — must include the last char 'f'.
        // (Previously the drag column was clamped to the *content* length 3,
        //  clipping 'f'.)
        let wrapped = vec!["↪ def".to_string()];
        let mut state = make_state(vec!["irrelevant"]);
        let mut mouse = make_mouse_state();
        mouse.selecting = true;
        mouse.click_mode = ClickMode::Char;
        mouse.anchor = Some((0, 2));
        update_selection(&mut state, &mut mouse, (0, 2), (0, 4), &wrapped, &[0]);
        let (sr, sc, er, ec) = state.selection.unwrap();
        assert_eq!(extract_selection(&wrapped, &[0], sr, sc, er, ec, false), "def");
    }

    #[test]
    fn test_line_select_continuation_row_includes_all_content() {
        // Line (triple-click) on a continuation row "↪ def" must select all of
        // "def", not just the first character.
        let wrapped = vec!["↪ def".to_string()];
        let mut state = make_state(vec!["irrelevant"]);
        let mut mouse = make_mouse_state();
        mouse.selecting = true;
        mouse.click_mode = ClickMode::Line;
        mouse.anchor = Some((0, 0));
        update_selection(&mut state, &mut mouse, (0, 0), (0, 4), &wrapped, &[0]);
        let (sr, sc, er, ec) = state.selection.unwrap();
        assert_eq!(extract_selection(&wrapped, &[0], sr, sc, er, ec, false), "def");
    }

    fn click_vp() -> ViewportCache {
        ViewportCache {
            log_area: ratatui::layout::Rect::new(0, 0, 80, 10),
            log_inner: ratatui::layout::Rect::new(0, 0, 80, 10),
            viewport_start: 0,
            total_wrapped: 1,
            wrapped: vec!["hello".to_string()],
            offsets: vec![0],
        }
    }

    #[test]
    fn test_alt_click_starts_rectangular_selection() {
        // Alt+drag = rectangular (column) selection, VS Code-style.
        use crossterm::event::{MouseButton, KeyModifiers, MouseEvent, MouseEventKind};
        let vp = click_vp();
        let mut state = make_state(vec!["hello"]);
        let mut mouse = make_mouse_state();
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 0,
            modifiers: KeyModifiers::ALT,
        };
        handle_mouse_event(ev, &mut state, &mut mouse, &vp);
        assert!(state.selection_rect, "Alt+click must start a rectangular selection");
    }

    #[test]
    fn test_plain_click_is_not_rectangular() {
        // A plain click (no modifiers) must NOT be rectangular — regression
        // guard against the old sticky-Ctrl behavior that flipped rect on.
        use crossterm::event::{MouseButton, KeyModifiers, MouseEvent, MouseEventKind};
        let vp = click_vp();
        let mut state = make_state(vec!["hello"]);
        let mut mouse = make_mouse_state();
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        handle_mouse_event(ev, &mut state, &mut mouse, &vp);
        assert!(!state.selection_rect, "plain click must not be rectangular");
    }

    #[cfg(feature = "tui")]
    #[test]
    fn test_wrap_with_offsets_matches_textwrap() {
        let cases = [
            "short",
            "the quick brown fox jumps over the lazy dog",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "hello   world  multiple   spaces",
            "trailing spaces   ",
            "  leading",
            "",
        ];
        for &text in &cases {
            for width in [3usize, 5, 8, 20] {
                let opts = textwrap::Options::new(width)
                    .subsequent_indent("↪ ")
                    .break_words(true);
                let expected: Vec<String> = textwrap::wrap(text, opts)
                    .into_iter()
                    .map(|c| c.into_owned())
                    .collect();
                let got: Vec<String> =
                    wrap_with_offsets(text, width, "↪ ").into_iter().map(|(r, _)| r).collect();
                assert_eq!(got, expected, "text={text:?} width={width}");
            }
        }
    }

    #[cfg(feature = "tui")]
    #[test]
    fn test_wrap_offsets_are_content_positions() {
        // "hello world" at width 8: the break consumes the space at char 5,
        // so the continuation row's content starts at char 6.
        let rows = wrap_with_offsets("hello world", 8, "↪ ");
        assert_eq!(rows[0], ("hello".to_string(), 0));
        assert_eq!(rows[1], ("↪ world".to_string(), 6));

        // Mid-word breaks (break_words) advance by the chunk size.
        let rows = wrap_with_offsets("abcdef", 2, "");
        assert_eq!(rows, vec![
            ("ab".to_string(), 0),
            ("cd".to_string(), 2),
            ("ef".to_string(), 4),
        ]);
    }

    /// Three logical lines wrapped at width 8:
    ///   "hello world" → ["hello", "↪ world"]  offsets [0, 6]
    ///   "short"       → ["short"]             offsets [0]
    ///   "firmi xyz"   → ["firmi", "↪ xyz"]    offsets [0, 6]
    fn rect_fixture() -> (Vec<String>, Vec<usize>) {
        (
            vec![
                "hello".to_string(),
                "↪ world".to_string(),
                "short".to_string(),
                "firmi".to_string(),
                "↪ xyz".to_string(),
            ],
            vec![0, 6, 0, 0, 6],
        )
    }

    #[test]
    fn test_rect_extract_selects_by_original_column() {
        // Box anchored at the START of a continuation (orig col 6) through
        // col 11: only continuation rows reach those columns; the first rows
        // ("hello", "short", "firmi" — all < 6 chars past 0... "hello" is 5,
        // "firmi" is 5, "short" is 5) contribute blank lines (box shape).
        let (wrapped, offsets) = rect_fixture();
        let text = extract_selection(&wrapped, &offsets, 1, 6, 4, 11, true);
        assert_eq!(text, "world\n\n\nxyz");
    }

    #[test]
    fn test_rect_extract_mid_continuation_only_same_depth() {
        // Columns inside the continuations' content: rows that don't reach the
        // original column contribute blank lines; "↪ xyz" is partially in.
        let (wrapped, offsets) = rect_fixture();
        let text = extract_selection(&wrapped, &offsets, 0, 7, 4, 10, true);
        assert_eq!(text, "\norl\n\n\nyz");
    }

    #[test]
    fn test_rect_extract_unwrapped_rows_behaves_like_display_columns() {
        // With no wrapping (offsets 0, no indent), content columns equal
        // display columns — same result as a display-based rectangle.
        let wrapped = vec!["abcde".to_string(), "fghij".to_string()];
        let text = extract_selection(&wrapped, &[0, 0], 0, 1, 1, 4, true);
        assert_eq!(text, "bcd\nghi");
    }

    #[test]
    fn test_rect_drag_stores_original_columns() {
        // Alt-drag from the first content char of row 1's continuation
        // (display col 2 → orig col 6) to display col 4 on row 4 (orig 8).
        let (wrapped, offsets) = rect_fixture();
        let mut state = make_state(vec!["irrelevant"]);
        let mut mouse = make_mouse_state();
        mouse.selecting = true;
        mouse.click_mode = ClickMode::Char;
        state.selection_rect = true;
        mouse.anchor = Some((1, 2));
        update_selection(&mut state, &mut mouse, (1, 2), (4, 4), &wrapped, &offsets);
        let (sr, sc, er, ec) = state.selection.unwrap();
        assert_eq!((sr, sc, er, ec), (1, 6, 4, 9));
        assert_eq!(
            extract_selection(&wrapped, &offsets, sr, sc, er, ec, true),
            "wor\n\n\nxyz"
        );
    }

    #[test]
    fn test_rect_render_highlights_by_original_column() {
        use ratatui::widgets::WidgetRef;
        let (wrapped, offsets) = rect_fixture();
        let widget = LogWidget {
            lines: wrapped,
            selection: Some((1, 6, 4, 9)),
            viewport_start: 0,
            rect: true,
            offsets,
        };
        let area = ratatui::layout::Rect::new(0, 0, 20, 5);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        widget.render_ref(area, &mut buf);
        let rev = |x: u16, y: u16| {
            buf.cell((x, y))
                .unwrap()
                .style()
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED)
        };
        // Row 1 "↪ world": orig 6..9 → display cols 2,3,4 highlighted only.
        assert!(rev(2, 1) && rev(4, 1));
        assert!(!rev(5, 1) && !rev(6, 1));
        // Row 4 "↪ xyz": orig 6..9 covers all of "xyz" → cols 2,3,4.
        assert!(rev(2, 4) && rev(4, 4));
        // Rows that don't reach orig col 6 ("hello", "short", "firmi").
        assert!(!rev(0, 0) && !rev(0, 2) && !rev(0, 3));
    }

    // ── Bug 2 test: content fits within bordered area ─────────────

    #[test]
    fn test_log_inner_area_excludes_borders() {
        // The LogWidget must be given an area that excludes the top/bottom borders.
        // With Margin { horizontal: 1, vertical: 1 }, a 10-row area gives 8 content rows.
        let area = ratatui::layout::Rect::new(0, 0, 80, 10);
        let inner = area.inner(ratatui::layout::Margin { horizontal: 1, vertical: 1 });
        assert_eq!(inner.y, 1, "inner should start below top border");
        assert_eq!(inner.height, 8, "inner height should exclude both borders");
        assert_eq!(inner.x, 1, "inner should start right of left border");
        assert_eq!(inner.width, 78, "inner width should exclude both borders");
    }

    // ── Bug 1 test: auto-scroll up during upward drag ─────────────

    #[test]
    fn test_auto_scroll_up_on_upward_drag() {
        // When dragging above the viewport, scroll_up should increase.
        // This tests the core logic: given a viewport and drag above,
        // scroll_up should change and selection should extend upward.
        let mut state = make_state(vec!["line1"; 100]);
        state.scroll_up = 5;

        let vp = ViewportCache {
            log_area: ratatui::layout::Rect::new(0, 0, 80, 10),
            log_inner: ratatui::layout::Rect::new(1, 1, 78, 8),
            viewport_start: 87, // 100 - 8 - 5
            total_wrapped: 100,
            wrapped: (0..100).map(|i| format!("line{i}")).collect(),
            offsets: vec![0; 100],
        };

        let mut mouse = make_mouse_state();
        mouse.selecting = true;
        mouse.click_mode = ClickMode::Char;
        mouse.anchor = Some((90, 3)); // Anchor somewhere in the middle

        // Simulate drag to row 0 (above inner.y=1)
        let scroll_before = state.scroll_up;
        let m = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: 10,
            row: 0, // Above viewport
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        handle_mouse_event(m, &mut state, &mut mouse, &vp);

        assert!(
            state.scroll_up > scroll_before,
            "auto-scroll up: scroll_up should increase (was {}, now {})",
            scroll_before, state.scroll_up
        );
        assert!(state.selection.is_some(), "selection should be extended upward");
    }

    #[test]
    fn test_auto_scroll_down_on_downward_drag() {
        // When dragging below the viewport, scroll_up should decrease.
        let mut state = make_state(vec!["line1"; 100]);
        state.scroll_up = 5;

        let vp = ViewportCache {
            log_area: ratatui::layout::Rect::new(0, 0, 80, 10),
            log_inner: ratatui::layout::Rect::new(1, 1, 78, 8),
            viewport_start: 87,
            total_wrapped: 100,
            wrapped: (0..100).map(|i| format!("line{i}")).collect(),
            offsets: vec![0; 100],
        };

        let mut mouse = make_mouse_state();
        mouse.selecting = true;
        mouse.click_mode = ClickMode::Char;
        mouse.anchor = Some((90, 3));

        let scroll_before = state.scroll_up;
        let m = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: 10,
            row: 10, // Below viewport (inner ends at row 8)
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        handle_mouse_event(m, &mut state, &mut mouse, &vp);

        assert!(
            state.scroll_up < scroll_before,
            "auto-scroll down: scroll_up should decrease (was {}, now {})",
            scroll_before, state.scroll_up
        );
        assert!(state.selection.is_some(), "selection should be extended downward");
    }

    // ── Bug: scrollbar drag above track underflows ────────────────

    #[test]
    fn test_scrollbar_drag_above_track_does_not_underflow() {
        // Press the scrollbar, then drag up along the scrollbar column past
        // the top of the track (row == area.y, the top border). The drag's
        // click_y computation (m.row - area.y - 1) must not underflow.
        // Found by the mouse_selection fuzz target.
        let mut state = make_state(vec!["line"; 100]);

        let vp = ViewportCache {
            log_area: ratatui::layout::Rect::new(0, 0, 80, 10),
            log_inner: ratatui::layout::Rect::new(1, 1, 78, 8),
            viewport_start: 92,
            total_wrapped: 100,
            wrapped: (0..100).map(|i| format!("line{i}")).collect(),
            offsets: vec![0; 100],
        };

        let mut mouse = make_mouse_state();
        let scrollbar_col = 79; // area.x + area.width - 1

        // Press on the scrollbar track.
        let press = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: scrollbar_col,
            row: 5,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        handle_mouse_event(press, &mut state, &mut mouse, &vp);
        assert!(mouse.scrollbar_drag, "press on scrollbar must start a scrollbar drag");

        // Drag along the scrollbar column up to the top border row.
        // Previously panicked: attempt to subtract with overflow.
        let drag = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: scrollbar_col,
            row: 0,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        handle_mouse_event(drag, &mut state, &mut mouse, &vp);

        // Dragging to the very top of the track shows the oldest content.
        let max_scroll = vp.total_wrapped.saturating_sub(vp.log_inner.height as usize);
        assert_eq!(
            state.scroll_up as usize, max_scroll,
            "dragging the scrollbar to the top must scroll to the oldest content"
        );
    }

    // ── Word boundary tests ───────────────────────────────────────

    #[test]
    fn test_word_boundaries_middle() {
        let line = "hello world foo";
        // Click on 'o' in 'world' (col 7)
        let (start, end) = word_boundaries(line, 7);
        assert_eq!(start, 6, "word start");
        assert_eq!(end, 11, "word end (exclusive)");
    }

    #[test]
    fn test_word_boundaries_at_space() {
        let line = "hello world";
        // Click on the space (col 5)
        let (start, end) = word_boundaries(line, 5);
        assert_eq!(start, 5, "space is its own word");
        assert_eq!(end, 6, "single char");
    }

    // ── Selection extraction tests ───────────────────────────────

    #[test]
    fn test_extract_multiline_no_continuation_newlines() {
        let wrapped = vec![
            "Hello World".to_string(),
            "↪ continuation".to_string(),
            "Next line".to_string(),
        ];
        // Select from row 0 col 0 to row 2 end
        let text = extract_selection(&wrapped, &[0, 12, 0], 0, 0, 2, 9, false);
        // Row 0 + continuation row 1 are the same original line: joined with the
        // space consumed at the wrap (no newline).
        // Row 2 is a new line → newline before it.
        assert_eq!(text, "Hello World continuation\nNext line");
    }

    #[test]
    fn test_extract_single_line() {
        let wrapped = vec!["Hello World".to_string()];
        let text = extract_selection(&wrapped, &[0], 0, 0, 0, 11, false);
        assert_eq!(text, "Hello World");
    }

    // ── Terminal size reactivity tests ───────────────────────────
    //
    // The layout must fill the entire terminal window and adapt to
    // different sizes. These tests verify the layout computation
    // produces the correct dimensions for small and large terminals.

    #[test]
    fn test_layout_fills_small_terminal() {
        let area = ratatui::layout::Rect::new(0, 0, 40, 10);
        let chunks = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([
                ratatui::layout::Constraint::Min(3),
                ratatui::layout::Constraint::Length(3),
            ])
            .split(area);

        let log_area = chunks[0];
        let log_inner = log_area.inner(ratatui::layout::Margin { horizontal: 1, vertical: 1 });

        assert_eq!(log_area.height, 7, "log area should be height-3");
        assert_eq!(log_area.width, 40, "log area should be full width");
        assert_eq!(log_inner.width, 38, "inner width = area - 2 borders");
        assert_eq!(log_inner.height, 5, "inner height = log_area - 2 borders");
    }

    #[test]
    fn test_layout_fills_large_terminal() {
        let area = ratatui::layout::Rect::new(0, 0, 200, 50);
        let chunks = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([
                ratatui::layout::Constraint::Min(3),
                ratatui::layout::Constraint::Length(3),
            ])
            .split(area);

        let log_area = chunks[0];
        let log_inner = log_area.inner(ratatui::layout::Margin { horizontal: 1, vertical: 1 });

        assert_eq!(log_area.height, 47, "log area should be height-3");
        assert_eq!(log_area.width, 200, "log area should be full width");
        assert_eq!(log_inner.width, 198);
        assert_eq!(log_inner.height, 45);
    }

    #[test]
    fn test_layout_adapts_between_sizes() {
        let compute = |w: u16, h: u16| {
            let area = ratatui::layout::Rect::new(0, 0, w, h);
            let chunks = ratatui::layout::Layout::default()
                .direction(ratatui::layout::Direction::Vertical)
                .constraints([
                    ratatui::layout::Constraint::Min(3),
                    ratatui::layout::Constraint::Length(3),
                ])
                .split(area);
            let log_inner = chunks[0].inner(ratatui::layout::Margin { horizontal: 1, vertical: 1 });
            (log_inner.width, log_inner.height)
        };

        let small = compute(40, 10);
        let large = compute(200, 50);

        assert_ne!(small, large, "layout must differ for different terminal sizes");
        assert_eq!(small, (38, 5));
        assert_eq!(large, (198, 45));
    }

    // ── Terminal size detection tests ────────────────────────────
    //
    // Inside nested containers (podman inside toolbox), ioctl(TIOCGWINSZ)
    // returns the intermediate PTY's size, not the real terminal size.
    // $COLUMNS and $LINES env vars propagate correctly through containers.

    #[test]
    fn test_size_from_env_vars() {
        assert_eq!(parse_terminal_size("200", "50"), Some((200, 50)));
    }

    #[test]
    fn test_size_from_env_missing() {
        // Empty strings simulate missing env vars (var().ok() returns None before this)
        assert_eq!(parse_terminal_size("", "50"), None);
        assert_eq!(parse_terminal_size("100", ""), None);
    }

    #[test]
    fn test_size_from_env_partial() {
        assert_eq!(parse_terminal_size("100", ""), None);
    }

    #[test]
    fn test_size_from_env_invalid() {
        assert_eq!(parse_terminal_size("not_a_number", "50"), None);
        assert_eq!(parse_terminal_size("100", "abc"), None);
    }

    #[test]
    fn test_size_from_env_zero() {
        assert_eq!(parse_terminal_size("0", "50"), None);
        assert_eq!(parse_terminal_size("100", "0"), None);
        assert_eq!(parse_terminal_size("0", "0"), None);
    }
}
