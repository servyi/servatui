//! Mouse state tracker: tracks position, button states, and modifiers.
//!
//! Handles edge cases:
//! - Mouse moving outside the terminal window during drag
//! - Button released outside the window (detect via focus loss or reset)
//! - Provides current state for rendering overlays/highlights

use crossterm::event::{MouseEvent, MouseEventKind, MouseButton, KeyModifiers};

/// Tracks the current state of the mouse across frames.
///
/// Update by calling `process()` for every `Event::Mouse` received.
/// Call `reset()` on focus loss to clear stale button states
/// (in case a release happened outside the window).
#[derive(Debug, Clone)]
pub struct MouseTracker {
    /// Last known mouse position (column, row) in screen coordinates.
    pub pos: (u16, u16),
    /// Whether each button is currently held down.
    pub left: bool,
    pub right: bool,
    pub middle: bool,
    /// Active keyboard modifiers at last mouse event.
    pub modifiers: KeyModifiers,
    /// Whether the mouse has moved since the last button press.
    pub moved_since_press: bool,
    /// Timestamp of last button-down (for double-click detection).
    pub last_down_time: Option<std::time::Instant>,
    /// Position of last button-down.
    pub last_down_pos: (u16, u16),
}

impl Default for MouseTracker {
    fn default() -> Self {
        Self {
            pos: (0, 0),
            left: false,
            right: false,
            middle: false,
            modifiers: KeyModifiers::NONE,
            moved_since_press: false,
            last_down_time: None,
            last_down_pos: (0, 0),
        }
    }
}

impl MouseTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process a mouse event and update internal state.
    pub fn process(&mut self, m: &MouseEvent) {
        self.pos = (m.column, m.row);
        self.modifiers = m.modifiers;

        match m.kind {
            MouseEventKind::Down(button) => {
                self.set_button(button, true);
                self.moved_since_press = false;
                self.last_down_time = Some(std::time::Instant::now());
                self.last_down_pos = (m.column, m.row);
            }
            MouseEventKind::Up(button) => {
                self.set_button(button, false);
            }
            MouseEventKind::Drag(button) => {
                self.set_button(button, true);
                if (m.column, m.row) != self.last_down_pos {
                    self.moved_since_press = true;
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {
                // Scroll events update position but not button state
            }
            MouseEventKind::Moved => {
                // Mouse moved without any button held
            }
        }
    }

    fn set_button(&mut self, button: MouseButton, down: bool) {
        match button {
            MouseButton::Left => self.left = down,
            MouseButton::Right => self.right = down,
            MouseButton::Middle => self.middle = down,
        }
    }

    /// Clear all button states. Call this when focus is lost
    /// (e.g., terminal window unfocused) to handle releases that
    /// happened outside the window.
    pub fn reset(&mut self) {
        self.left = false;
        self.right = false;
        self.middle = false;
    }

    /// True if any mouse button is currently held.
    pub fn any_down(&self) -> bool {
        self.left || self.right || self.middle
    }

    /// True if the last click was a double-click (same position, < 500ms).
    pub fn is_double_click(&self) -> bool {
        if let Some(t) = self.last_down_time {
            t.elapsed() < std::time::Duration::from_millis(500)
                && self.last_down_pos == self.pos
        } else {
            false
        }
    }

    /// Render a highlight at the current mouse position into the buffer.
    /// Only renders if the position is within the buffer bounds.
    pub fn render_highlight(&self, buf: &mut ratatui::buffer::Buffer, area: ratatui::layout::Rect) {
        use ratatui::style::{Color, Modifier, Style};

        let (mx, my) = self.pos;
        if mx < area.x || mx >= area.x + area.width || my < area.y || my >= area.y + area.height {
            return;
        }

        if let Some(cell) = buf.cell_mut((mx, my)) {
            let bg = if self.left {
                Color::LightGreen
            } else if self.right {
                Color::LightRed
            } else if self.middle {
                Color::LightBlue
            } else {
                Color::DarkGray
            };
            cell.set_style(Style::default().bg(bg).add_modifier(Modifier::BOLD));
        }
    }

    /// Return a compact status string: "L:● R:○ M:○ (col,row)".
    pub fn status_string(&self) -> String {
        format!(
            "L:{} R:{} M:{} ({},{})",
            if self.left { "●" } else { "○" },
            if self.right { "●" } else { "○" },
            if self.middle { "●" } else { "○" },
            self.pos.0, self.pos.1,
        )
    }
}
