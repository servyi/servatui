//! Modal textbox demo.
//!
//! Run: cargo run -p servatui-display --example textbox
//!
//! While open, the textbox swallows printable keys, Backspace, Enter and
//! Esc — nothing reaches the input line. Enter commits the text into the
//! title bar; the layer stays a taskbar/Shift+Tab target so `t` reopens it.

use crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, Paragraph};
use servatui_display::{Display, DisplayLayer, EventResult, LayerCtx, StackIntent};
use servyi_servatui::{Protocol, WidgetEntry};

struct TextBox {
    open: bool,
    buffer: String,
    committed: Option<String>,
}

impl TextBox {
    fn area(&self, terminal: Rect) -> Rect {
        let w = 50.min(terminal.width);
        Rect::new(
            terminal.x + (terminal.width - w) / 2,
            terminal.y + terminal.height / 2,
            w,
            3,
        )
    }
}

impl DisplayLayer for TextBox {
    fn tab_label(&self) -> char {
        'T'
    }

    fn on_overlay(&mut self, ctx: &mut LayerCtx, widgets: &mut Vec<WidgetEntry>) -> StackIntent {
        if !self.open {
                        return StackIntent::Keep;
        }
        let title = match &self.committed {
            Some(c) => format!(" Textbox — committed: {c} (t reopens) "),
            None => " Textbox — type, Enter commits, Esc cancels ".into(),
        };
        widgets.push(WidgetEntry {
            name: "demo.textbox",
            widget: Box::new(Paragraph::new(self.buffer.clone()).block(Block::default().borders(Borders::ALL).title(title))),
            area: self.area(ctx.terminal_area),
        });
    
    StackIntent::Keep
}

    fn on_event(&mut self, ev: &Event, _ctx: &LayerCtx) -> EventResult {
        let Event::Key(k) = ev else { return EventResult::Pass };
        if k.kind != KeyEventKind::Press {
            return EventResult::Pass;
        }
        match k.code {
            KeyCode::Char('t') if !self.open => {
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

fn main() {
    let mut display = Display::new();
    display.add_layer(Box::new(TextBox { open: true, buffer: String::new(), committed: None }));

    let protocols: Vec<Protocol> = vec![];
    display
        .run("/tmp/servatui-display-textbox-demo.sock".as_ref(), &protocols)
        .unwrap();
}
