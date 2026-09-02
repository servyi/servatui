//! servatui-display: window-manager layers for servatui TUIs.
//!
//! Built on top of servatui's closure API ([`servyi_servatui::run_tui_with_events`]):
//! a [`Display`] owns a list of [`DisplayLayer`]s, each with a unique id, a
//! color (mapped from the continuous layer ids), and a priority.
//!
//! # Frame lifecycle
//!
//! [`Display::frame`] runs inside the overlay closure each frame:
//! 1. the layer list is sorted by priority and priorities are collapsed to
//!    adjacent integers (ties stay tied);
//! 2. each layer's `on_overlay` contributes widgets in that order — later
//!    layers draw on top;
//! 3. widgets appearing during a layer's call are attributed to that layer
//!    (by name; the builtin widgets the core pushed beforehand are
//!    attributed to the builtin layer);
//! 4. before every owned widget, a rectangle of the layer's color is
//!    painted underneath it (except the builtin layer, to keep the default
//!    look);
//! 5. a one-cell taskbar strip is appended on the bottom terminal row, one
//!    colored cell per layer (including the builtin).
//!
//! # Event routing ([`Display::route_event`])
//!
//! Runs inside the event hook before servatui's builtin handling:
//! - **Shift+Tab** is system-reserved: it rotates activation through all
//!   layers, stepping down the stack and wrapping around.
//! - A press on a taskbar cell activates that layer.
//! - A layer that swallows a press becomes active (priority = max + 1) and
//!   grabs the pointer: subsequent drags and the release are delivered to
//!   it exclusively.
//! - Other mouse events hit-test topmost-first through the layers' owned
//!   widget areas; keys, resize and focus events are offered topmost-first
//!   to every layer (fall-through).
//! - If every layer passes, the event falls through to servatui's builtin
//!   handling (input line, log selection, scrollbar) unchanged.

use std::collections::{HashMap, HashSet};

use crossterm::event::Event;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Paragraph, WidgetRef};
use servyi_servatui::{WidgetEntry, WIDGET_INPUT};

/// Opaque, continuous layer identity. User layer ids start at 1; 0 is the
/// builtin bookkeeping layer ([`LayerId::BUILTIN`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerId(u64);

impl LayerId {
    /// The builtin layer's id (log/input bookkeeping, priority 0).
    pub const BUILTIN: LayerId = LayerId(0);
}

/// What a layer wants to do with an event it was offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventResult {
    /// Not mine — keep routing.
    Pass,
    /// Handled — stop routing (and, for presses: activate + grab).
    Swallow,
}

/// One window-manager layer.
pub trait DisplayLayer {
    /// Contribute widgets to this frame's stack. Widgets pushed here are
    /// attributed to this layer; render order is vec order, so layers run
    /// in priority order (higher priority draws later = on top).
    fn on_overlay(&mut self, ctx: &mut LayerCtx, widgets: &mut Vec<WidgetEntry>);

    /// Offer an event. Mouse events arrive only when the pointer is inside
    /// one of this layer's owned areas (or while it holds the grab);
    /// key/resize/focus events are offered to every layer, topmost first.
    fn on_event(&mut self, ev: &Event, ctx: &LayerCtx) -> EventResult {
        let _ = (ev, ctx);
        EventResult::Pass
    }

    /// Label for this layer's taskbar cell. `'\0'` (the default) falls back
    /// to a letter assigned by creation order.
    fn tab_label(&self) -> char {
        '\0'
    }
}

/// Per-layer context handed to [`DisplayLayer`] callbacks.
pub struct LayerCtx<'a> {
    /// This layer's id.
    pub id: LayerId,
    /// This layer's assigned color.
    pub color: Color,
    /// Best-effort full terminal area for this frame.
    pub terminal_area: Rect,
    /// The widget areas this layer owned at the end of the previous frame
    /// (empty on the first frame).
    pub my_widgets: &'a [(&'static str, Rect)],
}

impl LayerCtx<'_> {
    /// Whether a screen position lies inside one of this layer's widgets.
    pub fn hit_test(&self, col: u16, row: u16) -> bool {
        self.my_widgets.iter().any(|(_, area)| contains(area, col, row))
    }
}

/// Best-effort detection of how many colors the terminal supports
/// (`COLORTERM` for truecolor, `TERM` suffixes otherwise, 8 as fallback).
pub fn detect_color_count() -> usize {
    if let Ok(ct) = std::env::var("COLORTERM") {
        if ct.eq_ignore_ascii_case("truecolor") || ct.eq_ignore_ascii_case("24bit") {
            return 1 << 24;
        }
    }
    if let Ok(term) = std::env::var("TERM") {
        if term.contains("256color") {
            return 256;
        }
        if term.contains("16color") {
            return 16;
        }
    }
    8
}

/// The layer-color palette for a detected color count: with <= 8 colors all
/// available colors are used; with >= 16 colors the palette sticks to the
/// 8 darker shades so underlays and the taskbar stay muted. Layer colors
/// map directly onto the continuous layer ids (`id - 1` modulo palette).
pub fn palette_for(color_count: usize) -> Vec<Color> {
    let mut palette = [
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
        Color::Green,
        Color::Yellow,
        Color::Red,
    ]
    .to_vec();
    // <= 8 colors: use all of them. >= 16: stick to the 8 darker shades.
    palette.push(if color_count <= 8 { Color::Gray } else { Color::DarkGray });
    palette.push(Color::Black);
    palette
}

struct LayerSlot {
    id: LayerId,
    layer: Box<dyn DisplayLayer>,
    priority: u64,
    /// Insertion order — stable tie-break for the priority sort.
    seq: u64,
    label: char,
}

/// The window manager: layer list, priorities, ownership, routing.
pub struct Display {
    /// Sorted ascending by (priority, seq) at the start of every frame.
    slots: Vec<LayerSlot>,
    next_id: u64,
    palette: Vec<Color>,
    grabbed: Option<LayerId>,
    owners: HashMap<&'static str, LayerId>,
    cached_widgets: HashMap<LayerId, Vec<(&'static str, Rect)>>,
    taskbar: Vec<(LayerId, Rect)>,
    terminal_area: Rect,
    builtin_id: LayerId,
}

impl Default for Display {
    fn default() -> Self {
        Self::new()
    }
}

struct BuiltinLayer;

impl DisplayLayer for BuiltinLayer {
    fn on_overlay(&mut self, _ctx: &mut LayerCtx, _widgets: &mut Vec<WidgetEntry>) {}
}

struct NoopLayer;

impl DisplayLayer for NoopLayer {
    fn on_overlay(&mut self, _ctx: &mut LayerCtx, _widgets: &mut Vec<WidgetEntry>) {}
}

impl Display {
    /// New display with the palette for the detected terminal color count.
    pub fn new() -> Self {
        Self::with_palette(palette_for(detect_color_count()))
    }

    /// New display with an explicit palette (for tests).
    pub fn with_palette(palette: Vec<Color>) -> Self {
        let builtin_id = LayerId(0);
        Self {
            slots: vec![LayerSlot {
                id: builtin_id,
                layer: Box::new(BuiltinLayer),
                priority: 0,
                seq: 0,
                label: '*',
            }],
            next_id: 0,
            palette,
            grabbed: None,
            owners: HashMap::new(),
            cached_widgets: HashMap::new(),
            taskbar: Vec::new(),
            terminal_area: Rect::new(0, 0, 80, 24),
            builtin_id,
        }
    }

    /// Register a layer; it starts on top of the stack. Returns its id.
    pub fn add_layer(&mut self, mut layer: Box<dyn DisplayLayer>) -> LayerId {
        self.next_id += 1;
        let id = LayerId(self.next_id);
        let label = match layer.tab_label() {
            '\0' => (b'a' as u64 + (self.next_id - 1) % 26) as u8 as char,
            c => c,
        };
        let priority = self.slots.iter().map(|s| s.priority).max().unwrap_or(0) + 1;
        self.slots.push(LayerSlot { id, layer, priority, seq: self.next_id, label });
        id
    }

    /// Remove a layer (never the builtin layer).
    pub fn remove_layer(&mut self, id: LayerId) -> Option<Box<dyn DisplayLayer>> {
        if id == self.builtin_id {
            return None;
        }
        let idx = self.slots.iter().position(|s| s.id == id)?;
        let slot = self.slots.remove(idx);
        if self.grabbed == Some(id) {
            self.grabbed = None;
        }
        self.owners.retain(|_, owner| *owner != id);
        self.cached_widgets.remove(&id);
        Some(slot.layer)
    }

    /// Make a layer the topmost one (priority = max + 1). Re-sorts and
    /// collapses immediately so [`Display::topmost`]/[`Display::stack_order`]
    /// stay consistent without waiting for the next frame.
    pub fn activate(&mut self, id: LayerId) {
        let top = self.slots.iter().map(|s| s.priority).max().unwrap_or(0);
        if let Some(slot) = self.slots.iter_mut().find(|s| s.id == id) {
            slot.priority = top + 1;
        }
        self.resort();
    }

    /// Sort by (priority, insertion order) and collapse priorities to
    /// adjacent integers (ties stay tied).
    fn resort(&mut self) {
        self.slots.sort_by(|a, b| (a.priority, a.seq).cmp(&(b.priority, b.seq)));
        let mut group = 0u64;
        let mut last: Option<u64> = None;
        for slot in &mut self.slots {
            if last != Some(slot.priority) {
                if last.is_some() {
                    group += 1;
                }
                last = Some(slot.priority);
            }
            slot.priority = group;
        }
    }

    /// The currently topmost layer.
    pub fn topmost(&self) -> Option<LayerId> {
        self.slots.last().map(|s| s.id)
    }

    /// The stack in ascending priority order (bottom first). Reflects the
    /// last [`Display::frame`].
    pub fn stack_order(&self) -> Vec<LayerId> {
        self.slots.iter().map(|s| s.id).collect()
    }

    /// Which layer owns the widget with this name (per the last frame).
    pub fn owner_of(&self, name: &'static str) -> Option<LayerId> {
        self.owners.get(&name).copied()
    }

    /// The layer currently holding the pointer grab, if any.
    pub fn grabbed(&self) -> Option<LayerId> {
        self.grabbed
    }

    /// This layer's color (builtin layer has no palette color).
    pub fn layer_color(&self, id: LayerId) -> Option<Color> {
        if id == self.builtin_id || self.palette.is_empty() {
            return None;
        }
        Some(self.palette[((id.0 as usize).saturating_sub(1)) % self.palette.len()])
    }

    /// Run one frame inside servatui's overlay closure: sort + collapse
    /// priorities, run the layers' `on_overlay` in order, attribute new
    /// widget names, inject colored underlays, append the taskbar.
    pub fn frame(&mut self, widgets: &mut Vec<WidgetEntry>) {
        // Terminal area: the input box is the bottom layout chunk.
        if let Some(input) = widgets.iter().find(|w| w.name == WIDGET_INPUT) {
            self.terminal_area = Rect::new(0, 0, input.area.width, input.area.y + input.area.height);
        }

        // Sort by (priority, seq) and collapse priorities to adjacent
        // integers; ties stay tied. The builtin layer stays put at 0 unless
        // it was activated above others.
        self.resort();

        // Pre-existing names (the core's builtin widgets) belong to the
        // builtin layer unless a previous frame attributed them otherwise.
        for w in widgets.iter() {
            self.owners.entry(w.name).or_insert(self.builtin_id);
        }

        // Run each layer's overlay pass, attributing newly added names.
        for i in 0..self.slots.len() {
            let before: HashSet<&'static str> = widgets.iter().map(|w| w.name).collect();
            let (id, cached) = {
                let slot = &self.slots[i];
                (slot.id, self.cached_widgets.get(&slot.id).cloned().unwrap_or_default())
            };
            let color = self.layer_color(id).unwrap_or(Color::Reset);
            let mut layer = std::mem::replace(&mut self.slots[i].layer, Box::new(NoopLayer));
            let mut ctx =
                LayerCtx { id, color, terminal_area: self.terminal_area, my_widgets: &cached };
            layer.on_overlay(&mut ctx, widgets);
            self.slots[i].layer = layer;

            for w in widgets.iter() {
                if !before.contains(&w.name) {
                    self.owners.entry(w.name).or_insert(id);
                }
            }
        }

        // Refresh the per-layer widget caches from the final vec.
        self.cached_widgets.clear();
        for w in widgets.iter() {
            if let Some(owner) = self.owners.get(&w.name) {
                self.cached_widgets.entry(*owner).or_default().push((w.name, w.area));
            }
        }

        // Inject colored underlays below every non-builtin-owned widget.
        let mut decorated = Vec::with_capacity(widgets.len() * 2);
        for w in widgets.drain(..) {
            let owner = self.owners.get(&w.name).copied();
            if let Some(color) = owner.and_then(|o| self.layer_color(o)) {
                decorated.push(WidgetEntry {
                    name: "display.underlay",
                    widget: Box::new(Block::default().style(Style::default().bg(color))),
                    area: w.area,
                });
            }
            decorated.push(w);
        }
        *widgets = decorated;

        // Taskbar: one cell per layer on the bottom terminal row.
        self.taskbar.clear();
        let row = self.terminal_area.bottom().saturating_sub(1);
        let n = self.slots.len();
        for (i, slot) in self.slots.iter().enumerate() {
            let area = Rect::new(self.terminal_area.x + i as u16, row, 1, 1);
            let color = if slot.id == self.builtin_id {
                Color::DarkGray
            } else {
                self.layer_color(slot.id).unwrap_or(Color::DarkGray)
            };
            let mut style = Style::default().bg(color).fg(Color::Black);
            if i + 1 == n {
                style = style.add_modifier(Modifier::BOLD);
            }
            widgets.push(WidgetEntry {
                name: "display.taskbar",
                widget: Box::new(Paragraph::new(Span::styled(slot.label.to_string(), style))),
                area,
            });
            self.taskbar.push((slot.id, area));
        }
    }

    /// Route one terminal event. Returns `true` to swallow it (servatui's
    /// builtin handling is then skipped), `false` to let it fall through.
    /// Requires a previous [`Display::frame`] this session (routing uses
    /// the stack order and widget areas of the last frame).
    pub fn route_event(&mut self, ev: &Event) -> bool {
        use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};

        // Shift+Tab is system-reserved: rotate activation one step down the
        // stack (wrapping around), through all layers including the builtin.
        if let Event::Key(k) = ev {
            if k.kind == KeyEventKind::Press
                && k.code == KeyCode::Tab
                && k.modifiers.contains(KeyModifiers::SHIFT)
            {
                self.rotate();
                return true;
            }
        }

        // Presses on taskbar cells activate their layer.
        if let Event::Mouse(m) = ev {
            if let MouseEventKind::Down(_) = m.kind {
                if let Some((id, _)) =
                    self.taskbar.iter().find(|(_, area)| contains(area, m.column, m.row))
                {
                    let id = *id;
                    self.activate(id);
                    return true;
                }
            }
        }

        // The grabbed layer receives drags and the release exclusively.
        if let Some(grab_id) = self.grabbed {
            if let Event::Mouse(m) = ev {
                if matches!(m.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_)) {
                    let swallowed = self.deliver(grab_id, ev) == EventResult::Swallow;
                    if matches!(m.kind, MouseEventKind::Up(_)) {
                        self.grabbed = None;
                    }
                    return swallowed;
                }
            }
        }

        // Mouse events: hit-test through owned areas, topmost first.
        if let Event::Mouse(m) = ev {
            let is_down = matches!(m.kind, MouseEventKind::Down(_));
            for i in (0..self.slots.len()).rev() {
                let id = self.slots[i].id;
                let hit = self
                    .cached_widgets
                    .get(&id)
                    .is_some_and(|ws| ws.iter().any(|(_, a)| contains(a, m.column, m.row)));
                if !hit {
                    continue;
                }
                if self.deliver(id, ev) == EventResult::Swallow {
                    if is_down {
                        self.activate(id);
                        self.grabbed = Some(id);
                    }
                    return true;
                }
            }
            return false;
        }

        // Keys / resize / focus: offered to every layer, topmost first.
        for i in (0..self.slots.len()).rev() {
            if self.deliver(self.slots[i].id, ev) == EventResult::Swallow {
                return true;
            }
        }
        false
    }

    /// Run this display as the TUI client over servatui's event hook.
    pub fn run(self, socket: &std::path::Path, protocols: &[servyi_servatui::Protocol]) -> Result<(), String> {
        let display = std::cell::RefCell::new(self);
        servyi_servatui::run_tui_with_events(
            socket,
            protocols,
            |widgets| display.borrow_mut().frame(widgets),
            |ev| display.borrow_mut().route_event(ev),
        )
    }

    /// Shift+Tab rotation: a carousel — every press raises the bottom-most
    /// layer to the top, strictly cycling through all layers (builtin
    /// included). Deterministic and immune to the re-sorting activation
    /// performs ("next below the top" would ping-pong between two layers).
    fn rotate(&mut self) {
        if let Some(bottom) = self.slots.first().map(|s| s.id) {
            self.activate(bottom);
        }
    }

    fn deliver(&mut self, id: LayerId, ev: &Event) -> EventResult {
        let Some(idx) = self.slots.iter().position(|s| s.id == id) else {
            return EventResult::Pass;
        };
        let cached = self.cached_widgets.get(&id).cloned().unwrap_or_default();
        let color = self.layer_color(id).unwrap_or(Color::Reset);
        let mut layer = std::mem::replace(&mut self.slots[idx].layer, Box::new(NoopLayer));
        let ctx = LayerCtx { id, color, terminal_area: self.terminal_area, my_widgets: &cached };
        let result = layer.on_event(ev, &ctx);
        self.slots[idx].layer = layer;
        result
    }
}

fn contains(area: &Rect, col: u16, row: u16) -> bool {
    col >= area.x && col < area.x + area.width && row >= area.y && row < area.y + area.height
}
