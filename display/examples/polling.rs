//! Self-contained polling demo: the server generates a random number every
//! 2 seconds (background thread), the client polls it from a single
//! [`DisplayLayer`], and a popup lists every number the user has not yet
//! acknowledged.
//!
//! Run: cargo run -p servatui-display --example polling
//!
//! Everything runs in ONE process — the server is spawned on a background
//! thread, so no separate `serve` command is needed. The layer polls
//! inside `on_overlay` with a 2s throttle (idle frames run ~10x/s, so the
//! blocking localhost round-trip is fine here; for slower servers prefer a
//! poller thread publishing into shared state).
//!
//! The numbers popup carries two buttons: **[ ACK ]** acknowledges all
//! numbers and closes the popup — it is the ONLY way a click closes it —
//! and **[ DROP 1 ]** drops the oldest unseen number. Arrow keys move the
//! button focus (shown reversed), Enter presses the focused button. A
//! second popup with a notes textbox (`n` toggles, type, Enter commits,
//! Esc discards) partially overlaps the numbers popup: clicking either
//! raises it above the other, and Shift+Tab rotates the stacking.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use servatui_display::{Display, DisplayLayer, EventResult, LayerCtx};
use servyi_servatui::connection::TypedConnection;
use servyi_servatui::{
    BufferConsole, NoInput, Plugin, Protocol, ServerHandle, ShellAction, SocketConnection,
    WidgetEntry,
};

/// The protocol the client polls: the server answers with every number
/// generated so far.
fn numbers_protocol() -> Protocol {
    Plugin::new("numbers", "All numbers generated so far")
        .parse(|_args: &str| Ok(()))
        .client(|req: (), _out, _input| Ok(req))
        .server_ctx(|_req: (), ctx: &Mutex<Vec<u32>>| Ok(ctx.lock().unwrap().clone()))
        .client(|nums: Vec<u32>, out, _input| {
            for n in &nums {
                out.print_line(&n.to_string());
            }
            Ok(())
        })
        .finalize(|| Ok(ShellAction::Continue))
}

/// One DisplayLayer doing everything: throttled polling, popup rendering,
/// and acknowledgement.
struct PollLayer {
    socket: PathBuf,
    numbers: Vec<u32>,
    /// How many of `numbers` have been acknowledged or dropped.
    seen: usize,
    /// Focused button: 0 = [ ACK ], 1 = [ DROP 1 ].
    focus: usize,
    last_poll: Option<Instant>,
}

impl PollLayer {
    fn poll_now(&mut self) {
        self.last_poll = Some(Instant::now());
        let Ok(mut conn) = SocketConnection::connect(&self.socket) else { return };
        if conn.send_typed(&"numbers".to_string()).is_err() {
            return;
        }
        let mut console = BufferConsole::new();
        let mut input = NoInput;
        if let Ok(raw) = numbers_protocol().run_client("", &mut conn, &mut console, &mut input) {
            if let Ok(nums) = serde_json::from_slice::<Vec<u32>>(&raw) {
                self.numbers = nums;
            }
        }
    }
}

fn popup_area(terminal: Rect, lines: usize) -> Rect {
    let w = 36.min(terminal.width.saturating_sub(3));
    let h = (lines as u16 + 2).min(terminal.height.saturating_sub(1));
    // One cell down and one left, so our border does not sit on the log
    // widget's border.
    Rect::new(
        terminal.x + terminal.width.saturating_sub(w + 1),
        terminal.y + 1,
        w,
        h,
    )
}

/// Second popup: a notes textbox that partially overlaps the numbers popup.
/// Typing edits the buffer, Enter commits (and closes), Esc discards, `n`
/// reopens. Clicking it raises it above the numbers popup (and Shift+Tab
/// rotates the stacking the other way).
struct NotesLayer {
    open: bool,
    buffer: String,
    committed: Option<String>,
}

impl NotesLayer {
    fn new() -> Self {
        Self { open: true, buffer: String::new(), committed: None }
    }
}

fn notes_area(terminal: Rect) -> Rect {
    // Anchor relative to the numbers popup (x0 = width - 36 - 1): start 12
    // cells further left, three rows lower — the right part of this box
    // overlaps the left part of the numbers popup.
    let w = 30.min(terminal.width);
    let popup_x = terminal.width.saturating_sub(36 + 1);
    let x = popup_x.saturating_sub(12).min(terminal.width.saturating_sub(w));
    let y = 4.min(terminal.height.saturating_sub(1));
    let h = 3.min(terminal.height.saturating_sub(y));
    Rect::new(x, y, w, h)
}

impl DisplayLayer for NotesLayer {
    fn tab_label(&self) -> char {
        'T'
    }

    fn on_overlay(&mut self, ctx: &mut LayerCtx, widgets: &mut Vec<WidgetEntry>) {
        if !self.open {
            return;
        }
        let title = match &self.committed {
            Some(c) => format!(" notes: {c} — n reopens "),
            None => " notes — type, Enter commits, Esc discards ".to_string(),
        };
        widgets.push(WidgetEntry {
            name: "polling.notes",
            widget: Box::new(
                Paragraph::new(self.buffer.clone())
                    .block(Block::default().borders(Borders::ALL).title(title)),
            ),
            area: notes_area(ctx.terminal_area),
        });
    }

    fn on_event(&mut self, ev: &Event, _ctx: &LayerCtx) -> EventResult {
        if let Event::Mouse(m) = ev {
            // Clicks are only offered inside our area: swallow them while
            // open, which also activates us (raises over the numbers popup).
            if self.open && matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
                return EventResult::Swallow;
            }
            return EventResult::Pass;
        }
        let Event::Key(k) = ev else { return EventResult::Pass };
        if k.kind != KeyEventKind::Press {
            return EventResult::Pass;
        }
        match k.code {
            KeyCode::Char('n') if !self.open => {
                self.buffer.clear();
                self.open = true;
                EventResult::Swallow
            }
            _ if !self.open => EventResult::Pass,
            KeyCode::Char(c) => {
                self.buffer.push(c);
                EventResult::Swallow
            }
            KeyCode::Backspace => {
                self.buffer.pop();
                EventResult::Swallow
            }
            KeyCode::Enter => {
                self.committed = Some(std::mem::take(&mut self.buffer));
                self.open = false;
                EventResult::Swallow
            }
            KeyCode::Esc => {
                self.buffer.clear();
                self.open = false;
                EventResult::Swallow
            }
            _ => EventResult::Pass,
        }
    }
}

/// Screen rects of the two buttons on the popup's last inner row. Must
/// match what `button_row` renders.
fn button_rects(popup: Rect) -> (Rect, Rect) {
    let row = popup.y + popup.height.saturating_sub(2);
    let ack = Rect::new(popup.x + 1, row, 7, 1);
    let drop = Rect::new(popup.x + 11, row, 10, 1);
    (ack, drop)
}

fn in_rect(a: &Rect, col: u16, row: u16) -> bool {
    col >= a.x && col < a.x + a.width && row >= a.y && row < a.y + a.height
}

/// The [ ACK ] / [ DROP 1 ] button row; the focused button is reversed.
fn button_row(focus: usize) -> Line<'static> {
    let button = |label: &'static str, focused: bool| Span::styled(
        label,
        if focused {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        },
    );
    Line::from(vec![
        button("[ ACK ]", focus == 0),
        Span::raw("   "),
        button("[ DROP 1 ]", focus == 1),
    ])
}

impl DisplayLayer for PollLayer {
    fn tab_label(&self) -> char {
        'N'
    }

    fn on_overlay(&mut self, ctx: &mut LayerCtx, widgets: &mut Vec<WidgetEntry>) {
        // Poll at most every 2s (on_overlay runs ~10x/s while idle).
        let due = match self.last_poll {
            None => true,
            Some(t) => t.elapsed() >= Duration::from_secs(2),
        };
        if due {
            self.poll_now();
        }
        if self.numbers.len() > self.seen {
            let mut text = vec![Line::from(" new numbers — ←/→ then Enter, or click a button ")];
            text.extend(self.numbers[self.seen..].iter().map(|n| Line::from(format!("   {n}"))));
            text.push(button_row(self.focus));
            let n_lines = text.len();
            widgets.push(WidgetEntry {
                name: "polling.popup",
                widget: Box::new(
                    Paragraph::new(text).block(Block::default().borders(Borders::ALL)),
                ),
                area: popup_area(ctx.terminal_area, n_lines),
            });
        }
    }

    fn on_event(&mut self, ev: &Event, ctx: &LayerCtx) -> EventResult {
        if self.numbers.len() <= self.seen {
            return EventResult::Pass;
        }
        match ev {
            Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                // Arrows move focus between the two buttons; Enter presses
                // the focused one.
                KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                    self.focus = 1 - self.focus;
                    EventResult::Swallow
                }
                KeyCode::Enter => {
                    if self.focus == 0 {
                        // ACK: close the popup (acknowledge everything).
                        self.seen = self.numbers.len();
                    } else {
                        // DROP 1: drop the oldest unseen number.
                        self.seen = (self.seen + 1).min(self.numbers.len());
                    }
                    EventResult::Swallow
                }
                _ => EventResult::Pass,
            },
            Event::Mouse(m) if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) => {
                let Some((_, popup)) = ctx.my_widgets.iter().find(|(n, _)| *n == "polling.popup")
                else {
                    return EventResult::Pass;
                };
                let (ack_btn, drop_btn) = button_rects(*popup);
                if in_rect(&ack_btn, m.column, m.row) {
                    // Only ACK closes the window.
                    self.seen = self.numbers.len();
                } else if in_rect(&drop_btn, m.column, m.row) {
                    self.seen = (self.seen + 1).min(self.numbers.len());
                }
                // The click is ours either way (no close on other clicks).
                EventResult::Swallow
            }
            _ => EventResult::Pass,
        }
    }
}

fn main() {
    let socket =
        std::env::temp_dir().join(format!("servatui-polling-{}.sock", std::process::id()));

    // Server state: the generated numbers, appended by a generator thread.
    let numbers = Arc::new(Mutex::new(Vec::<u32>::new()));

    // Generator: a random number every 2 seconds (xorshift, no deps).
    {
        let numbers = numbers.clone();
        std::thread::spawn(move || {
            let mut state = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64
                | 1;
            loop {
                std::thread::sleep(Duration::from_secs(2));
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                numbers.lock().unwrap().push((state >> 32) as u32);
            }
        });
    }

    // Server: background thread in the same process.
    let handle = ServerHandle { socket: socket.clone(), protocols: vec![numbers_protocol()] };
    {
        let numbers = numbers.clone();
        std::thread::spawn(move || handle.run(numbers).ok());
    }
    for _ in 0..200 {
        if SocketConnection::server_exists(&socket) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    // Client: the TUI through the display manager.
    let mut display = Display::new();
    display.add_layer(Box::new(PollLayer {
        socket: socket.clone(),
        numbers: Vec::new(),
        seen: 0,
        focus: 0,
        last_poll: None,
    }));
    display.add_layer(Box::new(NotesLayer::new()));
    let result = display.run(&socket, &[numbers_protocol()]);

    let _ = std::fs::remove_file(&socket);
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
