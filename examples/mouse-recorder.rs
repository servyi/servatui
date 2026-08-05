//! Mouse event recorder with live tracker display.
//!
//! Run: cargo run --example mouse-recorder --features tui
//!
//! Follow on-screen instructions. The mouse position is highlighted in real-time.
//! Button states shown in footer. Press 'q' to quit and dump events.

use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind, MouseEvent, MouseEventKind, EnableMouseCapture, DisableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear},
    Terminal,
};
use servyi_servatui::MouseTracker;

#[derive(Debug, Clone)]
struct RecordedEvent {
    elapsed_ms: u128,
    kind: String,
    column: u16,
    row: u16,
    modifiers: String,
}

impl RecordedEvent {
    fn from_mouse(m: &MouseEvent, start: Instant) -> Self {
        let kind = match m.kind {
            MouseEventKind::Down(b) => format!("Down({:?})", b),
            MouseEventKind::Up(b) => format!("Up({:?})", b),
            MouseEventKind::Drag(b) => format!("Drag({:?})", b),
            MouseEventKind::ScrollDown => "ScrollDown".into(),
            MouseEventKind::ScrollUp => "ScrollUp".into(),
            MouseEventKind::ScrollLeft => "ScrollLeft".into(),
            MouseEventKind::ScrollRight => "ScrollRight".into(),
            MouseEventKind::Moved => "Moved".into(),
        };
        let modifiers = if m.modifiers.is_empty() { "none".into() } else { format!("{:?}", m.modifiers) };
        Self { elapsed_ms: start.elapsed().as_millis(), kind, column: m.column, row: m.row, modifiers }
    }
}

fn main() -> std::io::Result<()> {
    let instructions = vec![
        "1. Click once on the red 'X'",
        "2. Drag from green 'A' to blue 'B'",
        "3. Double-click on cyan 'HELLO'",
        "4. Scroll wheel down 2 notches",
        "5. Click on the scrollbar (rightmost col of border)",
        "6. Drag scrollbar to bottom",
        "7. Ctrl+drag from magenta 'C' to 'D'",
        "8. Right-click anywhere in log",
        "9. Middle-click anywhere in log",
        "DONE - press 'q' to dump events",
    ];

    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let start = Instant::now();
    let mut events: Vec<RecordedEvent> = Vec::new();
    let mut tracker = MouseTracker::new();
    let mut step: usize = 0;

    loop {
        let mut log_inner = Rect::default();
        let mut log_area = Rect::default();

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(8), Constraint::Length(3)])
                .split(f.area());

            log_area = chunks[0];
            log_inner = log_area.inner(ratatui::layout::Margin { horizontal: 1, vertical: 0 });

            f.render_widget(Clear, log_area);
            f.render_widget(
                Block::default().borders(Borders::ALL).title(format!(
                    "Log (inner: x={},y={},w={},h={} | scrollbar: col={})",
                    log_inner.x, log_inner.y, log_inner.width, log_inner.height,
                    log_area.x + log_area.width - 1,
                )),
                log_area,
            );

            let x = log_inner.x;
            let buf = f.buffer_mut();
            let mut row = log_inner.y;

            let mut line_at = |buf: &mut ratatui::buffer::Buffer, text: &str, style: Style| -> u16 {
                let y = row;
                buf.set_string(x, y, text, style);
                row += 1;
                y
            };

            // Instruction
            if step < instructions.len() {
                let y = line_at(buf, ">>> ", Style::default().fg(Color::Yellow));
                buf.set_string(x + 4, y, instructions[step], Style::default());
            }
            line_at(buf, "", Style::default());

            // Row 2: click X
            let y = line_at(buf, "  X click here", Style::default());
            buf.cell_mut((x + 2, y)).map(|c| c.set_char('X').set_style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)));

            // Row 3: drag A→B
            let y = line_at(buf, "  A........................................B", Style::default());
            buf.cell_mut((x + 2, y)).map(|c| c.set_char('A').set_style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)));
            buf.cell_mut((x + 44, y)).map(|c| c.set_char('B').set_style(Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)));

            // Row 4: double-click HELLO
            let y = line_at(buf, "  The word HELLO is here", Style::default());
            for (i, _) in "HELLO".chars().enumerate() {
                buf.cell_mut((x + 11 + i as u16, y)).map(|c| c.set_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)));
            }

            // Row 5: scroll
            line_at(buf, "  scroll wheel here", Style::default());

            // Row 6: ctrl+drag C→D
            let y = line_at(buf, "  C........................................D", Style::default());
            buf.cell_mut((x + 2, y)).map(|c| c.set_char('C').set_style(Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)));
            buf.cell_mut((x + 44, y)).map(|c| c.set_char('D').set_style(Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)));

            line_at(buf, "", Style::default());

            // Footer: tracker status + event count
            let status = format!("{}  |  Events: {}  |  Step: {}/{}",
                tracker.status_string(), events.len(), step + 1, instructions.len());
            line_at(buf, &status, Style::default().fg(Color::DarkGray));

            // Highlight current mouse position (overlays everything)
            let full_area = f.area();
            tracker.render_highlight(f.buffer_mut(), full_area);
        })?;

        if event::poll(Duration::from_millis(16))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    match k.code {
                        crossterm::event::KeyCode::Char('q') | crossterm::event::KeyCode::Char('Q') => break,
                        crossterm::event::KeyCode::Char('n') | crossterm::event::KeyCode::Enter
                            if step < instructions.len() - 1 => { step += 1; }
                        _ => {}
                    }
                }
                Event::Mouse(m) => {
                    tracker.process(&m);
                    events.push(RecordedEvent::from_mouse(&m, start));
                    match m.kind {
                        MouseEventKind::Up(_) | MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                            if step < instructions.len() - 1 => { step += 1; }
                        _ => {}
                    }
                }
                Event::FocusLost => {
                    tracker.reset();
                }
                _ => {}
            }
        }
    }

    execute!(std::io::stdout(), DisableMouseCapture, LeaveAlternateScreen)?;
    disable_raw_mode()?;

    // Dump
    println!("=== {} mouse events recorded ===\n", events.len());
    println!("{:>6}  {:<22}  {:>4}  {:>4}  {:>10}", "ms", "kind", "col", "row", "modifiers");
    println!("{}", "-".repeat(60));
    for e in &events {
        println!("{:>6}  {:<22}  {:>4}  {:>4}  {:>10}",
            e.elapsed_ms, e.kind, e.column, e.row, e.modifiers);
    }

    println!("\n=== Rust test replay data ===\n");
    println!("// (MouseEventKind, column, row)");
    println!("let events: &[(MouseEventKind, u16, u16)] = &[");
    for e in &events {
        let kind_code = match e.kind.as_str() {
            s if s.starts_with("Down(Left)") => "MouseEventKind::Down(MouseButton::Left)",
            s if s.starts_with("Up(Left)") => "MouseEventKind::Up(MouseButton::Left)",
            s if s.starts_with("Drag(Left)") => "MouseEventKind::Drag(MouseButton::Left)",
            s if s.starts_with("Down(Right)") => "MouseEventKind::Down(MouseButton::Right)",
            s if s.starts_with("Up(Right)") => "MouseEventKind::Up(MouseButton::Right)",
            s if s.starts_with("Down(Middle)") => "MouseEventKind::Down(MouseButton::Middle)",
            s if s.starts_with("Up(Middle)") => "MouseEventKind::Up(MouseButton::Middle)",
            "ScrollUp" => "MouseEventKind::ScrollUp",
            "ScrollDown" => "MouseEventKind::ScrollDown",
            _ => "MouseEventKind::Moved",
        };
        println!("    ({}, {}, {}), // +{}ms", kind_code, e.column, e.row, e.elapsed_ms);
    }
    println!("];");

    Ok(())
}
