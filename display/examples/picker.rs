//! List-picker demo: Up/Down to move, Enter to commit, Esc to cancel.
//!
//! Run: cargo run -p servatui-display --example picker
//!
//! The picker is a plain [`DisplayLayer`]: while open it swallows the
//! navigation keys, so they never reach the input line. The title bar shows
//! the committed choice.

use crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, Paragraph};
use servatui_display::{Display, DisplayLayer, EventResult, LayerCtx};
use servyi_servatui::{Protocol, WidgetEntry};

const OPTIONS: &[&str] = &["verify", "show", "status", "continue", "exit"];

struct Picker {
    open: bool,
    selected: usize,
    committed: Option<String>,
}

impl Picker {
    fn area(&self, terminal: Rect) -> Rect {
        let w = 30.min(terminal.width);
        let h = (OPTIONS.len() as u16 + 2).min(terminal.height);
        Rect::new(
            terminal.x + (terminal.width - w) / 2,
            terminal.y + (terminal.height - h) / 2,
            w,
            h,
        )
    }
}

impl DisplayLayer for Picker {
    fn tab_label(&self) -> char {
        'P'
    }

    fn on_overlay(&mut self, ctx: &mut LayerCtx, widgets: &mut Vec<WidgetEntry>) {
        if !self.open {
            return;
        }
        let title = match &self.committed {
            Some(c) => format!(" Picker — committed: {c} (r reopens) "),
            None => " Picker — ↑/↓ select, Enter commit, Esc cancel ".into(),
        };
        let items: String = OPTIONS
            .iter()
            .enumerate()
            .map(|(i, o)| {
                if i == self.selected {
                    format!("> {o} <\r\n")
                } else {
                    format!("  {o}  \r\n")
                }
            })
            .collect();
        widgets.push(WidgetEntry {
            name: "demo.picker",
            widget: Box::new(Paragraph::new(items).block(Block::default().borders(Borders::ALL).title(title))),
            area: self.area(ctx.terminal_area),
        });
    }

    fn on_event(&mut self, ev: &Event, _ctx: &LayerCtx) -> EventResult {
        let Event::Key(k) = ev else { return EventResult::Pass };
        if k.kind != KeyEventKind::Press {
            return EventResult::Pass;
        }
        match k.code {
            KeyCode::Char('r') => {
                self.open = true;
                EventResult::Swallow
            }
            KeyCode::Char('o') if !self.open => {
                self.open = true;
                EventResult::Swallow
            }
            _ if !self.open => EventResult::Pass,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                EventResult::Swallow
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(OPTIONS.len() - 1);
                EventResult::Swallow
            }
            KeyCode::Enter => {
                self.committed = Some(OPTIONS[self.selected].to_string());
                self.open = false;
                EventResult::Swallow
            }
            KeyCode::Esc => {
                self.open = false;
                EventResult::Swallow
            }
            _ => EventResult::Pass,
        }
    }
}

fn main() {
    let mut display = Display::new();
    display.add_layer(Box::new(Picker { open: true, selected: 0, committed: None }));

    let protocols: Vec<Protocol> = vec![];
    display
        .run("/tmp/servatui-display-picker-demo.sock".as_ref(), &protocols)
        .unwrap();
}
