//! End-to-end test of the poll-and-popup pattern demonstrated by
//! `examples/polling.rs`: a server answers with the numbers generated so
//! far, a single DisplayLayer polls it from `on_overlay`, and the popup
//! shows exactly the numbers the user has not yet acknowledged.
//!
//! Deterministic by construction: the test (not a timing-dependent
//! generator thread) decides when new numbers appear in the server state.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, Paragraph};
use servatui_display::{Display, DisplayLayer, EventResult, LayerCtx, StackIntent};
use servyi_servatui::connection::TypedConnection;
use servyi_servatui::{
    BufferConsole, NoInput, Plugin, Protocol, ServerHandle, ShellAction, SocketConnection,
    WidgetEntry, WIDGET_INPUT, WIDGET_LOG,
};

fn numbers_protocol() -> Protocol {
    Plugin::new("numbers", "test numbers")
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

/// Test flavor of the example's PollLayer: polls on EVERY frame (no
/// throttle) to keep the test fast.
struct EagerPollLayer {
    socket: PathBuf,
    numbers: Vec<u32>,
    seen: usize,
}

impl DisplayLayer for EagerPollLayer {
    fn on_overlay(&mut self, _ctx: &mut LayerCtx, widgets: &mut Vec<WidgetEntry>) -> StackIntent {
        let raw = SocketConnection::connect(&self.socket)
            .and_then(|mut conn| {
                conn.send_typed(&"numbers".to_string())?;
                numbers_protocol().run_client("", &mut conn, &mut BufferConsole::new(), &mut NoInput)
            })
            .unwrap_or_default();
        if let Ok(nums) = serde_json::from_slice::<Vec<u32>>(&raw) {
            self.numbers = nums;
        }
        if self.numbers.len() > self.seen {
            let lines: Vec<String> =
                self.numbers[self.seen..].iter().map(|n| format!("  {n}")).collect();
            widgets.push(WidgetEntry {
                name: "polling.popup",
                widget: Box::new(
                    Paragraph::new(lines.join("\r\n"))
                        .block(Block::default().borders(Borders::ALL).title(" new ")),
                ),
                area: Rect::new(40, 0, 30, 4),
            });
        }
    
    StackIntent::Keep
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
            Event::Mouse(m) if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) => {
                self.seen = self.numbers.len();
                EventResult::Swallow
            }
            _ => EventResult::Pass,
        }
    }
}

fn builtin_frame() -> Vec<WidgetEntry> {
    let w = |name: &'static str, area: Rect| WidgetEntry {
        name,
        widget: Box::new(Paragraph::new("x")),
        area,
    };
    vec![w(WIDGET_LOG, Rect::new(0, 0, 80, 21)), w(WIDGET_INPUT, Rect::new(0, 21, 80, 3))]
}

fn has_popup(frame: &[WidgetEntry]) -> Option<Rect> {
    frame.iter().find(|w| w.name == "polling.popup").map(|w| w.area)
}

fn down_event(col: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

#[test]
fn poll_popup_appears_acknowledges_and_reappears() {
    let socket =
        std::env::temp_dir().join(format!("servatui-poll-test-{}.sock", std::process::id()));

    // Server state with one number; the test adds more on demand.
    let numbers = Arc::new(Mutex::new(vec![7u32]));
    let handle = ServerHandle { socket: socket.clone(), protocols: vec![numbers_protocol()] };
    {
        let numbers = numbers.clone();
        std::thread::spawn(move || handle.run(numbers).ok());
    }
    for _ in 0..200 {
        if SocketConnection::server_exists(&socket) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let mut display = Display::with_palette(vec![ratatui::style::Color::Blue]);
    let layer_id = display.add_layer(Box::new(EagerPollLayer { socket: socket.clone(), numbers: Vec::new(), seen: 0 }));

    // First poll: one unseen number -> popup appears.
    let mut frame = builtin_frame();
    display.frame(&mut frame);
    has_popup(&frame).expect("unseen number must open the popup");

    // A click OUTSIDE the popup falls through (not acknowledged by stray
    // clicks elsewhere) — and focuses the builtin layer (click-to-focus).
    let stray = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 5,
        row: 15,
        modifiers: KeyModifiers::NONE,
    });
    assert!(!display.route_event(&stray), "click outside popup must fall through");
    let mut frame = builtin_frame();
    display.frame(&mut frame);
    assert!(has_popup(&frame).is_some(), "stray click must not acknowledge");

    // With the builtin focused, keystrokes go to the core's input line:
    // Esc no longer reaches the popup below.
    let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!display.route_event(&esc), "builtin topmost: Esc goes to the core");
    let mut frame = builtin_frame();
    display.frame(&mut frame);
    assert!(has_popup(&frame).is_some(), "Esc with builtin focused must not acknowledge");

    // Reactivating the popup via its taskbar button (slot 1 of 2: strip
    // start (80-7)/2 = 36, button at x 40) puts it back on top WITHOUT
    // acknowledging — and now Esc reaches it again.
    assert!(display.route_event(&down_event(41, 23)));
    assert_eq!(display.topmost(), Some(layer_id), "taskbar click reactivates the popup layer");
    assert!(display.route_event(&esc), "popup back on top: Esc reaches it again");
    let mut frame = builtin_frame();
    display.frame(&mut frame);
    assert!(has_popup(&frame).is_none(), "Esc must acknowledge");

    // A new number re-opens the popup; clicking inside acknowledges.
    numbers.lock().unwrap().push(42);
    let mut frame = builtin_frame();
    display.frame(&mut frame);
    let popup = has_popup(&frame).expect("new number must re-open the popup");
    let click = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: popup.x + 1,
        row: popup.y + 1,
        modifiers: KeyModifiers::NONE,
    });
    assert!(display.route_event(&click), "click inside popup must be swallowed");
    let mut frame = builtin_frame();
    display.frame(&mut frame);
    assert!(has_popup(&frame).is_none(), "click must acknowledge");

    let _ = std::fs::remove_file(&socket);
}
