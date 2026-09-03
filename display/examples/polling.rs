//! Self-contained polling demo: the server generates a random number every
//! 2 seconds (background thread), the client polls it from a single
//! [`DisplayLayer`], and a popup lists every number the user has not yet
//! acknowledged (Esc or a click).
//!
//! Run: cargo run -p servatui-display --example polling
//!
//! Everything runs in ONE process — the server is spawned on a background
//! thread, so no separate `serve` command is needed. The layer polls
//! inside `on_overlay` with a 2s throttle (idle frames run ~10x/s, so the
//! blocking localhost round-trip is fine here; for slower servers prefer a
//! poller thread publishing into shared state).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
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
    /// How many of `numbers` the user has acknowledged (Esc / click).
    seen: usize,
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
    let w = 36.min(terminal.width.saturating_sub(2));
    let h = (lines as u16 + 2).min(terminal.height);
    Rect::new(terminal.x + terminal.width.saturating_sub(w), terminal.y, w, h)
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
            let mut lines = vec![" new numbers — Esc or click acknowledges ".to_string()];
            lines.extend(self.numbers[self.seen..].iter().map(|n| format!("  {n}")));
            widgets.push(WidgetEntry {
                name: "polling.popup",
                widget: Box::new(
                    Paragraph::new(lines.join("\r\n"))
                        .block(Block::default().borders(Borders::ALL)),
                ),
                area: popup_area(ctx.terminal_area, lines.len()),
            });
        }
    }

    fn on_event(&mut self, ev: &Event, _ctx: &LayerCtx) -> EventResult {
        if self.numbers.len() <= self.seen {
            return EventResult::Pass;
        }
        match ev {
            Event::Key(k) if k.kind == KeyEventKind::Press && k.code == KeyCode::Esc => {
                self.seen = self.numbers.len();
                EventResult::Swallow
            }
            // The router only offers mouse events inside our popup area.
            Event::Mouse(m) if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) => {
                self.seen = self.numbers.len();
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
        last_poll: None,
    }));
    let result = display.run(&socket, &[numbers_protocol()]);

    let _ = std::fs::remove_file(&socket);
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
