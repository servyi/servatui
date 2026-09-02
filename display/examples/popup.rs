//! Clickable, colored popup demo.
//!
//! Run: cargo run -p servatui-display --example popup
//!
//! - `p` toggles the second popup; `Esc` closes the popup that is topmost
//! - clicking a popup activates it (colored underlay + bold taskbar cell)
//! - clicking outside a popup's visible box closes it (full-screen modal)
//! - Shift+Tab rotates through the layers; the taskbar cells are clickable
//! - typing `exit` still works: keys no popup swallows reach the input line

use crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders};
use servatui_display::{Display, DisplayLayer, EventResult, LayerCtx};
use servyi_servatui::{Protocol, WidgetEntry};

struct Popup {
    widget_name: &'static str,
    title: &'static str,
    open: bool,
    /// 'p' toggles this popup (None for the static one).
    toggle_key: Option<char>,
}

impl Popup {
    fn visible_area(&self, terminal: Rect) -> Rect {
        let w = 40.min(terminal.width);
        let h = 8.min(terminal.height);
        Rect::new(
            terminal.x + (terminal.width - w) / 2,
            terminal.y + (terminal.height - h) / 2,
            w,
            h,
        )
    }
}

impl DisplayLayer for Popup {
    fn on_overlay(&mut self, _ctx: &mut LayerCtx, widgets: &mut Vec<WidgetEntry>) {
        if !self.open {
            return;
        }
        // Full-screen entry => modal: clicks outside the visible box still
        // reach this layer and close it. The colored underlay the Display
        // paints behind the full-screen entry is mostly hidden by the block.
        widgets.push(WidgetEntry {
            name: self.widget_name,
            widget: Box::new(Block::default().borders(Borders::ALL).title(self.title)),
            area: self.visible_area(_ctx.terminal_area),
        });
    }

    fn on_event(&mut self, ev: &Event, ctx: &LayerCtx) -> EventResult {
        match ev {
            Event::Key(k)
                if k.kind == KeyEventKind::Press && Some(k.code) == self.toggle_key.map(KeyCode::Char) =>
            {
                self.open = !self.open;
                EventResult::Swallow
            }
            Event::Key(k) if k.kind == KeyEventKind::Press && k.code == KeyCode::Esc && self.open => {
                self.open = false;
                EventResult::Swallow
            }
            Event::Mouse(m)
                if self.open && matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) =>
            {
                let v = self.visible_area(ctx.terminal_area);
                let inside = m.column >= v.x
                    && m.column < v.x + v.width
                    && m.row >= v.y
                    && m.row < v.y + v.height;
                if !inside {
                    self.open = false;
                }
                EventResult::Swallow
            }
            _ => EventResult::Pass,
        }
    }
}

fn main() {
    let mut display = Display::new();
    display.add_layer(Box::new(Popup {
        widget_name: "demo.popup.info",
        title: " Info — Esc closes, Shift+Tab rotates, p opens the second ",
        open: true,
        toggle_key: None,
    }));
    display.add_layer(Box::new(Popup {
        widget_name: "demo.popup.second",
        title: " Second — click outside me to dismiss ",
        open: false,
        toggle_key: Some('p'),
    }));

    let protocols: Vec<Protocol> = vec![];
    display
        .run("/tmp/servatui-display-popup-demo.sock".as_ref(), &protocols)
        .unwrap();
}
