//! TUI shell: widget list + overlay callback + textwrap rendering.
//!
//! servatui builds its default widgets each frame, calls an overlay
//! callback so the user can add/remove/inspect, then renders all widgets.
//! Uses alternate scroll mode (`\x1b[?1007h`) for native text selection.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

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

/// Log lines and scroll state.
pub struct TuiState {
    pub log_lines: Vec<String>,
    pub scroll_up: u16,
}

impl Default for TuiState {
    fn default() -> Self { Self::new() }
}

impl TuiState {
    pub fn new() -> Self {
        Self {
            log_lines: vec!["servatui interactive mode. Type 'help' for commands.".into()],
            scroll_up: 0,
        }
    }
}

/// A widget that renders pre-wrapped log lines into a ratatui buffer.
/// Continuation lines (starting with "↪ ") have the prefix styled gray.
struct LogWidget {
    lines: Vec<String>,
}

impl ratatui::widgets::WidgetRef for LogWidget {
    fn render_ref(&self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        use ratatui::style::{Color, Modifier, Style};

        let gray = Style::default().fg(Color::DarkGray);

        for (row, line) in self.lines.iter().enumerate() {
            if row >= area.height as usize { break; }
            let is_cont = line.starts_with("↪ ");
            let y = area.y + row as u16;

            for (col, ch) in line.chars().enumerate() {
                if col >= area.width as usize { break; }
                let x = area.x + col as u16;
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(ch);
                    if is_cont && col < 2 {
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
/// The callback receives `&mut Vec<WidgetEntry>` each frame, after servatui
/// has added its default widgets but before rendering. The user can
/// inspect, add, remove, or reorder widgets.
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
    use ratatui::{backend::CrosstermBackend, Terminal};
    use tui_input::Input;

    enable_raw_mode().map_err(|e| e.to_string())?;
    execute!(std::io::stdout(), EnterAlternateScreen).map_err(|e| e.to_string())?;
    write!(std::io::stdout(), "\x1b[?1007h").map_err(|e| e.to_string())?;
    std::io::stdout().flush().ok();

    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend).map_err(|e| e.to_string())?;

    let mut state = TuiState::new();
    let mut input = Input::default();
    let mut history: Vec<String> = Vec::new();
    let mut history_idx: Option<usize> = None;

    let result = tui_loop(
        &mut terminal, &mut state, &mut input,
        &mut history, &mut history_idx,
        socket, protocols, &mut on_overlay,
    );

    write!(std::io::stdout(), "\x1b[?1007l").ok();
    std::io::stdout().flush().ok();
    disable_raw_mode().ok();
    execute!(std::io::stdout(), LeaveAlternateScreen).ok();
    println!("Goodbye.");
    result
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
) -> Result<(), String> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    use ratatui::{
        layout::{Constraint, Direction, Layout},
        widgets::{Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, WidgetRef},
    };
    use textwrap::{Options, wrap};
    use tui_input::backend::crossterm::EventHandler;

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
            let visible: Vec<String> = wrapped[start..].to_vec();

            // Clear and render log border
            f.render_widget(Clear, log_area);
            f.render_widget(Block::default().borders(Borders::ALL).title(title), log_area);

            // Build widget list
            let mut widgets: Vec<WidgetEntry> = Vec::new();

            widgets.push(WidgetEntry {
                name: WIDGET_LOG,
                widget: Box::new(LogWidget { lines: visible }),
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

            // Overlay callback — user can inspect/add/remove widgets
            on_overlay(&mut widgets);

            // Render all widgets via WidgetRef
            for entry in &widgets {
                entry.widget.render_ref(entry.area, f.buffer_mut());
            }

            // Scrollbar (StatefulWidget, rendered separately)
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

            // Cursor position in input pane
            f.set_cursor_position((
                input_area.x + 1 + prompt.len() as u16 + input.visual_cursor() as u16,
                input_area.y + 1,
            ));
        }).map_err(|e| e.to_string())?;

        if event::poll(Duration::from_secs(3)).map_err(|e| e.to_string())? {
            let ev = event::read().map_err(|e| e.to_string())?;
            let key = match ev {
                Event::Key(k) if k.kind == KeyEventKind::Press => k,
                _ => continue,
            };

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
                        continue;
                    }

                    history.push(line.clone());
                    state.log_lines.push(format!("> {trimmed}"));
                    state.scroll_up = 0;

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
