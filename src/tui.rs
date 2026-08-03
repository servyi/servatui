//! TUI shell: widget list + overlay callback + mouse selection.
//!
//! Mouse capture is enabled for:
//! - Click-drag text selection in the log area (character, word, line, block modes)
//! - Scrollbar dragging (click + drag to scroll)
//! - Scroll wheel for scrolling
//! - Selection persists and extends during scroll/auto-scroll
//! Selection is copied to clipboard via OSC 52 on mouse release.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::console::Console;
use crate::connection::{SocketConnection, TypedConnection};
use crate::protocol::Protocol;

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
        }
    }
}

/// Selection mode based on click count.
#[derive(Clone, Copy, PartialEq)]
enum ClickMode { Char, Word, Line }

/// Tracks mouse interaction state across frames.
struct MouseState {
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
    /// Whether Ctrl is held (for rectangular selection).
    ctrl_held: bool,
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
            ctrl_held: false,
        }
    }
}

/// Characters that are part of a "word" (matching VTE/gnome-terminal defaults).
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || "-./:%_@~".contains(c)
}

/// Find word boundaries around col in the given line.
fn word_boundaries(line: &str, col: usize) -> (usize, usize) {
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() || col >= chars.len() {
        return (col, col);
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

/// Copy text to clipboard via OSC 52 escape sequence.
fn osc52_copy(text: &str) {
    use base64::{Engine, engine::general_purpose};
    let encoded = general_purpose::STANDARD.encode(text);
    let _ = write!(std::io::stdout(), "\x1b]52;c;{}\x1b\\", encoded);
    let _ = std::io::stdout().flush();
}

/// Paste from clipboard via OSC 52 read request.
/// The terminal responds with the clipboard content, which arrives as terminal
/// input. This sends the request; the response needs to be handled separately.
fn osc52_paste() {
    let _ = write!(std::io::stdout(), "\x1b]52;c;?\x1b\\");
    let _ = std::io::stdout().flush();
}

/// Extract selected text from all wrapped lines (content coordinates).
/// Strips ↪ prefixes from continuation lines and joins appropriately.
fn extract_selection(
    wrapped: &[String],
    sr: usize, sc: usize, er: usize, ec: usize,
    rect: bool,
) -> String {
    let (sr, sc, er, ec) = if (sr, sc) > (er, ec) { (er, ec, sr, sc) } else { (sr, sc, er, ec) };
    let mut result = String::new();

    if rect {
        // Rectangular: each row contributes the same column range
        let (cmin, cmax) = (sc.min(ec), sc.max(ec));
        for row in sr..=er {
            if row >= wrapped.len() { break; }
            let line = wrapped[row].strip_prefix("↪ ").unwrap_or(&wrapped[row]);
            let chars: Vec<char> = line.chars().collect();
            let s = cmin.min(chars.len());
            let e = cmax.min(chars.len());
            if s < e { result.extend(&chars[s..e]); }
            if row < er { result.push('\n'); }
        }
    } else {
        // Normal: start-to-end contiguous selection
        for row in sr..=er {
            if row >= wrapped.len() { break; }
            let line = wrapped[row].strip_prefix("↪ ").unwrap_or(&wrapped[row]);
            let chars: Vec<char> = line.chars().collect();
            let col_start = if row == sr { sc.min(chars.len()) } else { 0 };
            let col_end = if row == er { ec.min(chars.len()) } else { chars.len() };
            if col_start < col_end { result.extend(&chars[col_start..col_end]); }
            if row < er { result.push('\n'); }
        }
    }
    result
}

/// A widget that renders pre-wrapped log lines with optional selection highlight.
struct LogWidget {
    lines: Vec<String>,
    /// Selection in content coordinates.
    selection: Option<(usize, usize, usize, usize)>,
    /// Content row of the first visible line (viewport offset).
    viewport_start: usize,
    /// Whether selection is rectangular.
    rect: bool,
}

impl ratatui::widgets::WidgetRef for LogWidget {
    fn render_ref(&self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        use ratatui::style::{Color, Modifier, Style};

        let gray = Style::default().fg(Color::DarkGray);

        // Translate selection to viewport coordinates
        let sel_vp = self.selection.map(|(sr, sc, er, ec)| {
            let vsr = sr.saturating_sub(self.viewport_start);
            let ver = er.saturating_sub(self.viewport_start);
            let ordered = if (sr, sc) > (er, ec) { (er, ec, sr, sc) } else { (sr, sc, er, ec) };
            (ordered.0.saturating_sub(self.viewport_start), ordered.1,
             ordered.2.saturating_sub(self.viewport_start), ordered.3)
        });

        for (row, line) in self.lines.iter().enumerate() {
            if row >= area.height as usize { break; }
            let is_cont = line.starts_with("↪ ");
            let y = area.y + row as u16;
            let chars: Vec<char> = line.chars().collect();

            for (col, ch) in chars.iter().enumerate() {
                if col >= area.width as usize { break; }
                let x = area.x + col as u16;

                let is_selected = match sel_vp {
                    Some((sr, sc, er, ec)) if self.rect => {
                        // Rectangular: same column range for all rows
                        let (cmin, cmax) = (sc.min(ec), sc.max(ec));
                        row >= sr && row <= er && col >= cmin && col < cmax
                    }
                    Some((sr, sc, er, ec)) => {
                        // Normal contiguous
                        if row > sr && row < er { true }
                        else if row == sr && row == er { col >= sc && col < ec }
                        else if row == sr { col >= sc }
                        else if row == er { col < ec }
                        else { false }
                    }
                    _ => false,
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
#[cfg(feature = "tui")]
pub fn run_tui(socket: &Path, protocols: &[Protocol]) -> Result<(), String> {
    run_tui_with_overlay(socket, protocols, |_| {})
}

/// Run the TUI client with an overlay callback.
#[cfg(feature = "tui")]
pub fn run_tui_with_overlay<F>(
    socket: &Path,
    protocols: &[Protocol],
    mut on_overlay: F,
) -> Result<(), String>
where
    F: FnMut(&mut Vec<WidgetEntry>),
{
    use crossterm::execute;
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use crossterm::event::{EnableMouseCapture, DisableMouseCapture};
    use ratatui::{backend::CrosstermBackend, Terminal};
    use tui_input::Input;

    enable_raw_mode().map_err(|e| e.to_string())?;
    execute!(std::io::stdout(), EnterAlternateScreen, EnableMouseCapture).map_err(|e| e.to_string())?;
    std::io::stdout().flush().ok();

    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend).map_err(|e| e.to_string())?;

    let mut state = TuiState::new();
    let mut input = Input::default();
    let mut history: Vec<String> = Vec::new();
    let mut history_idx: Option<usize> = None;
    let mut mouse = MouseState::default();

    let result = tui_loop(
        &mut terminal, &mut state, &mut input,
        &mut history, &mut history_idx,
        socket, protocols, &mut on_overlay, &mut mouse,
    );

    execute!(std::io::stdout(), DisableMouseCapture, LeaveAlternateScreen).ok();
    disable_raw_mode().ok();
    println!("Goodbye.");
    result
}

/// Cached viewport info for mouse coordinate translation.
struct ViewportCache {
    log_area: ratatui::layout::Rect,
    log_inner: ratatui::layout::Rect,
    viewport_start: usize,
    total_wrapped: usize,
    wrapped: Vec<String>,
    visible: Vec<String>,
}

#[cfg(feature = "tui")]
fn tui_loop(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    state: &mut TuiState,
    input: &mut tui_input::Input,
    history: &mut Vec<String>,
    history_idx: &mut Option<usize>,
    socket: &Path,
    protocols: &[Protocol],
    on_overlay: &mut dyn FnMut(&mut Vec<WidgetEntry>),
    mouse: &mut MouseState,
) -> Result<(), String> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind, MouseButton};
    use ratatui::{
        layout::{Constraint, Direction, Layout},
        widgets::{Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, WidgetRef},
    };
    use textwrap::{Options, wrap};
    use tui_input::backend::crossterm::EventHandler;

    let mut vp: Option<ViewportCache> = None;

    loop {
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(3)])
                .split(f.area());

            let log_area = chunks[0];
            let log_inner = log_area.inner(ratatui::layout::Margin { horizontal: 1, vertical: 0 });
            let log_height = log_inner.height as usize;
            let inner_width = log_inner.width as usize;

            // Pre-wrap log lines
            let wrapped: Vec<String> = state.log_lines.iter()
                .flat_map(|s| {
                    let clean = s.rsplit_once('\r').map(|(_, last)| last).unwrap_or(s);
                    let clean = clean.replace('\t', "    ");
                    let opts = Options::new(inner_width)
                        .subsequent_indent("↪ ")
                        .break_words(true);
                    wrap(&clean, opts).into_iter().map(|cow| cow.into_owned()).collect::<Vec<_>>()
                })
                .collect();

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
                visible: visible.clone(),
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
                }),
                area: log_inner,
            });

            let prompt = "> ";
            let input_area = chunks[1];
            widgets.push(WidgetEntry {
                name: WIDGET_INPUT,
                widget: Box::new(
                    Paragraph::new(format!("{prompt}{}", input.value()))
                        .block(Block::default().borders(Borders::ALL).title("Input")),
                ),
                area: input_area,
            });

            on_overlay(&mut widgets);

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

            match ev {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    mouse.ctrl_held = key.modifiers.contains(KeyModifiers::CONTROL);
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
                            input.reset(); *history_idx = None; continue;
                        }
                        KeyCode::Tab => {
                            let word = input.value().split_whitespace().next().unwrap_or("");
                            if !word.is_empty() && !input.value().contains(' ') {
                                if let Some(p) = protocols.iter().find(|p| p.name.starts_with(word)) {
                                    *input = tui_input::Input::new(p.name.into());
                                }
                            }
                            continue;
                        }
                        KeyCode::Enter => {
                            let line = input.value().to_string();
                            input.reset();
                            *history_idx = None;
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

                            history.push(line.clone());
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
                            if !history.is_empty() {
                                *history_idx = Some(match *history_idx {
                                    Some(0) => 0,
                                    Some(i) => i - 1,
                                    None => history.len() - 1,
                                });
                                *input = tui_input::Input::new(history[history_idx.unwrap()].clone());
                            }
                        }
                        KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            match *history_idx {
                                Some(i) if i + 1 < history.len() => {
                                    let new_idx = i + 1;
                                    *history_idx = Some(new_idx);
                                    *input = tui_input::Input::new(history[new_idx].clone());
                                }
                                _ => { *history_idx = None; input.reset(); }
                            }
                        }
                        _ => { let _ = input.handle_event(&Event::Key(key)); }
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
                            update_selection(state, mouse, anchor, (new_start, 0), &vp.wrapped);
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
                            update_selection(state, mouse, anchor, (bottom_row, usize::MAX), &vp.wrapped);
                        }
                    }
                }
            }
        }
    }
}

/// Update selection based on anchor + current position.
fn update_selection(
    state: &mut TuiState,
    mouse: &mut MouseState,
    anchor: (usize, usize),
    current: (usize, usize),
    wrapped: &[String],
) {
    // Clamp current col to line width
    let cur_row = current.0.min(wrapped.len().saturating_sub(1));
    let line_len = if cur_row < wrapped.len() {
        wrapped[cur_row].strip_prefix("↪ ").unwrap_or(&wrapped[cur_row]).chars().count()
    } else { 0 };
    let cur_col = current.1.min(line_len);

    match mouse.click_mode {
        ClickMode::Char => {
            state.selection = Some((anchor.0, anchor.1, cur_row, cur_col + 1));
        }
        ClickMode::Word => {
            // Word mode: expand both anchor and current to word boundaries
            if cur_row < wrapped.len() {
                let (ws, we) = word_boundaries(&wrapped[cur_row], cur_col);
                state.selection = Some((anchor.0, anchor.1, cur_row, we));
            }
        }
        ClickMode::Line => {
            // Line mode: select entire rows from anchor to current
            state.selection = Some((anchor.0, 0, cur_row, line_len));
        }
    }

    // Rectangular mode: flag for rendering
    state.selection_rect = mouse.ctrl_held && mouse.click_mode == ClickMode::Char;
}

fn handle_mouse_event(
    m: crossterm::event::MouseEvent,
    state: &mut TuiState,
    mouse: &mut MouseState,
    vp: &ViewportCache,
) {
    use crossterm::event::{MouseEventKind, MouseButton};

    let inner = vp.log_inner;
    let area = vp.log_area;

    // Check position zones
    let in_log = m.column >= inner.x
        && m.column < inner.x + inner.width
        && m.row >= inner.y
        && m.row < inner.y + inner.height;

    let scrollbar_col = area.x + area.width - 1;
    let on_scrollbar = m.column == scrollbar_col
        && m.row >= area.y + 1
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
        MouseEventKind::ScrollUp => {
            if in_log || on_scrollbar {
                state.scroll_up = state.scroll_up.saturating_add(3);
            }
        }
        MouseEventKind::ScrollDown => {
            if in_log || on_scrollbar {
                state.scroll_up = state.scroll_up.saturating_sub(3);
            }
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
            match mouse.click_mode {
                ClickMode::Char => {
                    mouse.anchor = Some((crow, ccol));
                    state.selection_rect = mouse.ctrl_held;
                    state.selection = Some((crow, ccol, crow, ccol + 1));
                }
                ClickMode::Word => {
                    if crow < vp.wrapped.len() {
                        let line = vp.wrapped[crow].strip_prefix("↪ ").unwrap_or(&vp.wrapped[crow]);
                        let (ws, we) = word_boundaries(line, ccol);
                        mouse.anchor = Some((crow, ws));
                        state.selection = Some((crow, ws, crow, we));
                    }
                }
                ClickMode::Line => {
                    let line_len = if crow < vp.wrapped.len() {
                        vp.wrapped[crow].strip_prefix("↪ ").unwrap_or(&vp.wrapped[crow]).chars().count()
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
                    let click_y = (m.row - area.y - 1) as usize;
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
                mouse.auto_scroll = Some(true); // scroll up (older)
                return;
            } else if m.row >= inner.y + inner.height {
                mouse.auto_scroll = Some(false); // scroll down (newer)
                return;
            } else {
                mouse.auto_scroll = None;
            }

            // Normal drag within viewport
            let crow = content_row(m.row);
            let ccol = content_col(m.column);
            if let Some(anchor) = mouse.anchor {
                update_selection(state, mouse, anchor, (crow, ccol), &vp.wrapped);
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
                    let text = extract_selection(&vp.wrapped, sr, sc, er, ec, state.selection_rect);
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
                    update_selection(state, mouse, anchor, (crow, ccol), &vp.wrapped);
                    // Copy on extend
                    if let Some((sr, sc, er, ec)) = state.selection {
                        let text = extract_selection(&vp.wrapped, sr, sc, er, ec, state.selection_rect);
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
    let mut conn = SocketConnection::connect(socket)?;
    conn.send_typed(&proto.name.to_string())?;
    let mut console = TuiBufferConsole::new();
    let mut input_src = crate::console::NoInput;
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
