//! TUI shell: two-window layout (scrollable log + input), Console/InputSource impls.
//!
//! Uses alternate scroll mode (`\x1b[?1007h`) so scroll wheel and text selection
//! both work without mouse capture. See tui-instructions.txt for full design.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use crate::console::Console;
use crate::connection::{SocketConnection, TypedConnection};
use crate::protocol::Protocol;

/// Log lines and scroll state for the TUI.
pub struct TuiState {
    pub log_lines: Vec<String>,
    pub scroll_up: u16,
}

impl Default for TuiState {
    fn default() -> Self {
        Self::new()
    }
}

impl TuiState {
    pub fn new() -> Self {
        Self {
            log_lines: vec!["servatui interactive mode. Type 'help' for commands.".into()],
            scroll_up: 0,
        }
    }
}

/// Run the TUI client. Blocks until user types 'exit' or Ctrl-D.
#[cfg(feature = "tui")]
pub fn run_tui(socket: &Path, protocols: &[Protocol]) -> Result<(), String> {
    
    use crossterm::execute;
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use ratatui::{
        backend::CrosstermBackend,
        Terminal,
    };
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

    let result = tui_loop(&mut terminal, &mut state, &mut input, &mut history, &mut history_idx, socket, protocols);

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
) -> Result<(), String> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    use ratatui::{
        layout::{Constraint, Direction, Layout},
        text::Line,
        widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
    };
    use tui_input::backend::crossterm::EventHandler;

    loop {
        // Clamp scroll
        let term_h = terminal.size().map(|s| s.height as usize).unwrap_or(24);
        let log_h = term_h.saturating_sub(3).saturating_sub(2);
        let max_scroll = state.log_lines.len().saturating_sub(log_h);
        state.scroll_up = (state.scroll_up as usize).min(max_scroll) as u16;

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(3)])
                .split(f.area());

            let log_height = chunks[0].height.saturating_sub(2) as usize;
            let total = state.log_lines.len();
            let scroll = total.saturating_sub(log_height).saturating_sub(state.scroll_up as usize);

            let title = if state.scroll_up > 0 {
                format!("Log (↑{} lines)", state.scroll_up)
            } else {
                "Log".to_string()
            };

            let lines: Vec<Line> = state.log_lines.iter().map(|s| Line::from(s.as_str())).collect();
            f.render_widget(
                Paragraph::new(lines)
                    .scroll((scroll as u16, 0))
                    .block(Block::default().borders(Borders::ALL).title(title))
                    .wrap(Wrap { trim: false }),
                chunks[0],
            );

            // Scrollbar
            let ms = total.saturating_sub(log_height);
            let sb_scroll = ms.saturating_sub(state.scroll_up as usize);
            let mut sb_state = ScrollbarState::new(ms + 1)
                .position(sb_scroll)
                .viewport_content_length(log_height);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None),
                chunks[0].inner(ratatui::layout::Margin { horizontal: 0, vertical: 1 }),
                &mut sb_state,
            );

            // Input pane
            let prompt = "> ";
            f.render_widget(
                Paragraph::new(format!("{prompt}{}", input.value()))
                    .block(Block::default().borders(Borders::ALL).title("Input")),
                chunks[1],
            );
            f.set_cursor_position((
                chunks[1].x + 1 + prompt.len() as u16 + input.visual_cursor() as u16,
                chunks[1].y + 1,
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
                KeyCode::Up if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    state.scroll_up = state.scroll_up.saturating_add(1);
                    continue;
                }
                KeyCode::Down if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    state.scroll_up = state.scroll_up.saturating_sub(1);
                    continue;
                }
                KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) && input.value().is_empty() => {
                    return Ok(());
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    input.reset();
                    *history_idx = None;
                    continue;
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

                    // Execute command
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
                    match history_idx {
                        Some(i) if *i + 1 < history.len() => {
                            let new_idx = *i + 1;
                            *history_idx = Some(new_idx);
                            *input = tui_input::Input::new(history[new_idx].clone());
                        }
                        _ => {
                            *history_idx = None;
                            input.reset();
                        }
                    }
                }
                _ => { let _ = input.handle_event(&Event::Key(key)); }
            }
        }
    }
}

/// Execute a command: connect to server, run client side, collect output.
fn execute_command(proto: &Protocol, args: &str, socket: &Path) -> Result<Vec<String>, String> {
    let mut conn = SocketConnection::connect(socket)?;
    conn.send_typed(&proto.name.to_string())?;
    let mut console = TuiBufferConsole::new();
    let mut input_src = crate::console::NoInput;
    proto.run_client(args, &mut conn, &mut console, &mut input_src)?;
    Ok(console.lines)
}

/// Console that collects lines into a Vec (for TUI log area).
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
