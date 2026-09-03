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
//!    layers draw on top — and states a [`StackIntent`] (`Top`, `Bottom`,
//!    `Keep`) for where it wants to sit next frame;
//! 3. widgets appearing during a layer's call are attributed to that layer
//!    (by name; the builtin widgets the core pushed beforehand are
//!    attributed to the builtin layer), and the vec is regrouped so each
//!    layer's widgets are consecutive and the groups appear in stack
//!    order — activating the builtin layer really brings the log/input to
//!    the front;
//! 4. before each group, one backdrop rect per widget (never interleaved
//!    with them) clears the cells and paints the layer color (the builtin
//!    group clears to the terminal default), so lower layers cannot bleed
//!    through — necessary now that the builtin can rise above others;
//! 5. a taskbar strip is appended on the bottom terminal row: one
//!    3-cells-wide button per layer (including the builtin), 1 space
//!    between buttons, horizontally centered, at stable slot positions
//!    assigned by id and recycled when layers are removed.
//!
//! # Event routing ([`Display::route_event`])
//!
//! Runs inside the event hook before servatui's builtin handling:
//! - **Shift+Tab** is system-reserved: it rotates activation through all
//!   layers, stepping down the stack and wrapping around.
//! - A press on a taskbar button activates that layer.
//! - Mouse events stop at the **visual occupant**: the topmost layer whose
//!   area contains the point gets the event and lower layers never see it
//!   (a click on a popup covered by the builtin belongs to the builtin). A
//!   layer that swallows a press becomes active (priority = max + 1) and
//!   grabs the pointer: subsequent drags and the release are delivered to
//!   it exclusively. If the occupant passes a press, it is still
//!   activated (**click-to-focus**) and the event falls through to the
//!   core — which is how clicking the log/input activates the builtin.
//! - Other mouse events hit-test topmost-first through the layers' owned
//!   widget areas; keys, resize and focus events are offered topmost-first
//!   to every layer (fall-through).
//! - If every layer passes, the event falls through to servatui's builtin
//!   handling (input line, log selection, scrollbar) unchanged.
//!
//! # Focus
//!
//! Activation-based routing implements a kind of **focus**: the active
//! (topmost) layer is offered key input first, and **any input swallowed
//! by the focused layer is not given to any other layer** — nor to
//! servatui's builtin handling. Swallowing means consumption.
//!
//! The complementary rules:
//! - A key the focused layer *passes* falls through to the layers below
//!   (useful for background hotkey layers), and if every layer passes, the
//!   core's builtin input line handles it.
//! - When the *builtin* layer is the active one, keys fall straight
//!   through to the core's input line — the input line is the focused
//!   target and user layers below cannot intercept.
//! - Mouse input is not focus-based but spatial: the visual occupant (the
//!   topmost layer covering the click point) receives it exclusively.
//!
//! Focus follows activation: taskbar clicks, Shift+Tab rotation, swallowed
//! presses, click-to-focus, and [`StackIntent::Top`] all change which
//! layer is offered keys first (and fire [`DisplayLayer::on_active`] on
//! the newly focused layer).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crossterm::event::Event;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
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
    /// Handled — the event is consumed: no other layer and no builtin
    /// handling will see it. For presses this also activates the layer
    /// (it becomes the focused one) and grabs the pointer.
    Swallow,
}

/// What a layer wants to happen to its stack position after its
/// [`DisplayLayer::on_overlay`] call: rise to the top, sink to the bottom,
/// or keep its current priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackIntent {
    Keep,
    Top,
    Bottom,
}

/// One window-manager layer.
pub trait DisplayLayer {
    /// Contribute widgets to this frame's stack. Widgets pushed here are
    /// attributed to this layer; render order is vec order, so layers run
    /// in priority order (higher priority draws later = on top).
    ///
    /// The return value states where the layer wants to sit on the next
    /// frame: [`StackIntent::Top`] rises it above everyone, `Bottom` sinks
    /// it below everyone, `Keep` leaves the priority as it is.
    fn on_overlay(&mut self, ctx: &mut LayerCtx, widgets: &mut Vec<WidgetEntry>) -> StackIntent;

    /// Called when this layer becomes the topmost one (activation), e.g.
    /// via a taskbar click, Shift+Tab rotation, a swallowed press, or a
    /// [`StackIntent::Top`] request. Not fired for layers that merely get
    /// added on top.
    fn on_active(&mut self) {}

    /// If `true`, the layer is hidden from the taskbar and cannot be
    /// clicked (its content has no areas anyway) while it owns no widgets —
    /// without losing its reserved taskbar slot.
    fn hide_when_empty(&self) -> bool {
        false
    }

    /// Offer an event. Mouse events arrive only when the pointer is inside
    /// one of this layer's owned areas (or while it holds the grab);
    /// key/resize/focus events are offered to every layer, topmost first —
    /// the active layer gets them first (see the crate docs' *Focus*
    /// section). Returning [`EventResult::Swallow`] consumes the event:
    /// no other layer and no builtin handling will see it.
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

struct LayerSlot<'a> {
    id: LayerId,
    layer: Box<dyn DisplayLayer + 'a>,
    priority: u64,
    /// Insertion order — stable tie-break for the priority sort.
    seq: u64,
    label: char,
}

/// The window manager: layer list, priorities, ownership, routing.
pub struct Display<'a> {
    /// Sorted ascending by (priority, seq) at the start of every frame.
    slots: Vec<LayerSlot<'a>>,
    next_id: u64,
    palette: Vec<Color>,
    grabbed: Option<LayerId>,
    owners: HashMap<&'static str, LayerId>,
    cached_widgets: HashMap<LayerId, Vec<(&'static str, Rect)>>,
    taskbar: Vec<(LayerId, Rect)>,
    terminal_area: Rect,
    builtin_id: LayerId,
    /// The shared builtin TUI, attached by [`Display::run`] so the builtin
    /// layer handles events through the very state the loop renders.
    builtin_tui: Option<Rc<RefCell<servyi_servatui::tui::BuiltinTui<'a>>>>,
    /// Stable taskbar slot per layer; freed slots are recycled.
    slot_of: HashMap<LayerId, usize>,
    free_slots: Vec<usize>,
    next_slot: usize,
}

/// The backdrop behind a layer's widget group: clears every cell in its
/// area and paints the layer color. Unlike a styled `Block`, this resets
/// symbols, so content from lower layers cannot bleed through.
struct LayerBackdrop {
    color: Color,
}

impl ratatui::widgets::WidgetRef for LayerBackdrop {
    fn render_ref(&self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.reset();
                    cell.set_bg(self.color);
                }
            }
        }
    }
}

impl Default for Display<'_> {
    fn default() -> Self {
        Self::new()
    }
}

/// The builtin layer: drives the SHARED [`BuiltinTui`] (log, input line,
/// scrollbar and all their behavior) owned by the event loop. Without an
/// attached instance it passes everything, and the core handles events on
/// fall-through — the headless-test configuration.
struct BuiltinLayer<'a> {
    tui: Option<Rc<RefCell<servyi_servatui::tui::BuiltinTui<'a>>>>,
}

impl DisplayLayer for BuiltinLayer<'_> {
    fn on_overlay(&mut self, _ctx: &mut LayerCtx, _widgets: &mut Vec<WidgetEntry>) -> StackIntent {
        StackIntent::Keep
    }

    fn on_event(&mut self, ev: &Event, _ctx: &LayerCtx) -> EventResult {
        let Some(tui) = &self.tui else { return EventResult::Pass };
        if tui.borrow_mut().handle_event(ev) {
            EventResult::Swallow
        } else {
            EventResult::Pass
        }
    }
}

struct NoopLayer;

impl DisplayLayer for NoopLayer {
    fn on_overlay(&mut self, _ctx: &mut LayerCtx, _widgets: &mut Vec<WidgetEntry>) -> StackIntent {
        StackIntent::Keep
    }
}

impl<'a> Display<'a> {
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
                layer: Box::new(BuiltinLayer { tui: None }),
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
            builtin_tui: None,
            slot_of: HashMap::from([(builtin_id, 0)]),
            free_slots: Vec::new(),
            next_slot: 1,
        }
    }

    /// Attach the shared builtin TUI (done automatically by
    /// [`Display::run`]): from then on the builtin layer handles events
    /// through the same state the event loop renders.
    pub fn attach_builtin(&mut self, tui: Rc<RefCell<servyi_servatui::tui::BuiltinTui<'a>>>) {
        self.builtin_tui = Some(tui.clone());
        if let Some(slot) = self.slots.iter_mut().find(|s| s.id == self.builtin_id) {
            slot.layer = Box::new(BuiltinLayer { tui: Some(tui) });
        }
    }

    /// Register a layer; it starts on top of the stack. Returns its id.
    pub fn add_layer(&mut self, layer: Box<dyn DisplayLayer>) -> LayerId {
        self.next_id += 1;
        let id = LayerId(self.next_id);
        let label = match layer.tab_label() {
            '\0' => (b'a' as u64 + (self.next_id - 1) % 26) as u8 as char,
            c => c,
        };
        // Stable taskbar slot: recycle a freed one if available.
        let taskbar_slot = match self.free_slots.pop() {
            Some(s) => s,
            None => {
                let s = self.next_slot;
                self.next_slot += 1;
                s
            }
        };
        self.slot_of.insert(id, taskbar_slot);
        let priority = self.slots.iter().map(|s| s.priority).max().unwrap_or(0) + 1;
        self.slots.push(LayerSlot { id, layer, priority, seq: self.next_id, label });
        id
    }

    /// Remove a layer (never the builtin layer).
    pub fn remove_layer(&mut self, id: LayerId) -> Option<Box<dyn DisplayLayer + 'a>> {
        if id == self.builtin_id {
            return None;
        }
        let idx = self.slots.iter().position(|s| s.id == id)?;
        let slot = self.slots.remove(idx);
        if self.grabbed == Some(id) {
            self.grabbed = None;
        }
        if let Some(taskbar_slot) = self.slot_of.remove(&id) {
            self.free_slots.push(taskbar_slot);
        }
        self.owners.retain(|_, owner| *owner != id);
        self.cached_widgets.remove(&id);
        Some(slot.layer)
    }

    /// Make a layer the topmost one (priority = max + 1). Re-sorts and
    /// collapses immediately so [`Display::topmost`]/[`Display::stack_order`]
    /// stay consistent without waiting for the next frame. Fires
    /// [`DisplayLayer::on_active`] on the layer.
    pub fn activate(&mut self, id: LayerId) {
        let top = self.slots.iter().map(|s| s.priority).max().unwrap_or(0);
        if let Some(slot) = self.slots.iter_mut().find(|s| s.id == id) {
            slot.priority = top + 1;
        }
        self.resort();
        self.notify_active(id);
    }

    /// Deliver the "became active" event to a layer (swap-out pattern —
    /// the slots vec is otherwise borrowed immutably by callers).
    fn notify_active(&mut self, id: LayerId) {
        let Some(idx) = self.slots.iter().position(|s| s.id == id) else {
            return;
        };
        let mut layer = std::mem::replace(&mut self.slots[idx].layer, Box::new(NoopLayer));
        layer.on_active();
        self.slots[idx].layer = layer;
    }

    /// Apply the stack intents collected during this frame's `on_overlay`
    /// passes: `Top` requesters rise above everyone, `Bottom` requesters
    /// sink below everyone, relative order otherwise preserved; priorities
    /// renumbered. Fires `on_active` if the topmost layer changed.
    fn apply_intents(&mut self, intents: &HashMap<LayerId, StackIntent>) {
        if intents.is_empty() {
            return;
        }
        let prev_top = self.slots.last().map(|s| s.id);
        let old = std::mem::take(&mut self.slots);
        let mut bottoms = Vec::with_capacity(old.len());
        let mut keeps = Vec::with_capacity(old.len());
        let mut tops = Vec::with_capacity(old.len());
        for slot in old {
            match intents.get(&slot.id) {
                Some(StackIntent::Top) => tops.push(slot),
                Some(StackIntent::Bottom) => bottoms.push(slot),
                _ => keeps.push(slot),
            }
        }
        self.slots = bottoms;
        self.slots.extend(keeps);
        self.slots.extend(tops);
        for (p, slot) in self.slots.iter_mut().enumerate() {
            slot.priority = p as u64;
        }
        if self.slots.last().map(|s| s.id) != prev_top {
            if let Some(id) = self.slots.last().map(|s| s.id) {
                self.notify_active(id);
            }
        }
    }

    /// Sort by (priority, insertion order) and collapse priorities to
    /// adjacent integers (ties stay tied).
    fn resort(&mut self) {
        self.slots.sort_by_key(|s| (s.priority, s.seq));
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

    /// The collapsed priorities, ascending, matching [`Display::stack_order`].
    pub fn priorities(&self) -> Vec<u64> {
        self.slots.iter().map(|s| s.priority).collect()
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

        // Run each layer's overlay pass, attributing newly added names and
        // collecting stack intents.
        let mut intents: HashMap<LayerId, StackIntent> = HashMap::new();
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
            let intent = layer.on_overlay(&mut ctx, widgets);
            self.slots[i].layer = layer;
            intents.insert(id, intent);

            for w in widgets.iter() {
                if !before.contains(&w.name) {
                    self.owners.entry(w.name).or_insert(id);
                }
            }
        }

        // Apply the requested stack moves for this frame's ordering.
        self.apply_intents(&intents);

        // Refresh the per-layer widget caches from the final vec.
        self.cached_widgets.clear();
        for w in widgets.iter() {
            if let Some(owner) = self.owners.get(&w.name) {
                self.cached_widgets.entry(*owner).or_default().push((w.name, w.area));
            }
        }

        // Regroup the vec: each layer's widgets become consecutive, and the
        // groups appear in stack order (bottom first). The builtin widgets
        // the core pushed first therefore draw on top once the builtin
        // layer is activated above user layers.
        let mut groups: HashMap<LayerId, Vec<WidgetEntry>> = HashMap::new();
        let mut appearance: Vec<LayerId> = Vec::new();
        for w in widgets.drain(..) {
            let owner = self.owners.get(&w.name).copied().unwrap_or(self.builtin_id);
            if !groups.contains_key(&owner) {
                appearance.push(owner);
            }
            groups.entry(owner).or_default().push(w);
        }
        let mut order: Vec<LayerId> =
            self.slots.iter().map(|s| s.id).filter(|id| groups.contains_key(id)).collect();
        for id in appearance {
            if !order.contains(&id) {
                order.push(id); // defensive: an owner without a live slot
            }
        }
        for id in order {
            if let Some(group) = groups.remove(&id) {
                widgets.extend(group);
            }
        }

        // One backdrop rect per widget of each non-builtin group, all
        // inserted before the group's first widget — never interleaved with
        // the widgets, so a layer's own overlapping widgets composite
        // correctly. Backdrops clear the cells and paint the layer color.
        let mut decorated = Vec::with_capacity(widgets.len() * 2);
        let mut group: Vec<WidgetEntry> = Vec::new();
        let mut group_owner: Option<LayerId> = None;
        for w in widgets.drain(..) {
            let owner = self.owners.get(&w.name).copied().unwrap_or(self.builtin_id);
            if group_owner != Some(owner) {
                self.flush_group(&mut decorated, &mut group, group_owner);
                group_owner = Some(owner);
            }
            group.push(w);
        }
        self.flush_group(&mut decorated, &mut group, group_owner);
        *widgets = decorated;

        // Taskbar: one button per layer on the bottom terminal row, at a
        // STABLE slot position (assigned by id, recycled on removal — not
        // the priority order), 3 cells wide with a 1-column gap, the strip
        // centered horizontally. The topmost layer's button is bold.
        self.taskbar.clear();
        let row = self.terminal_area.bottom().saturating_sub(1);
        let total_slots = self.slot_of.values().copied().max().map_or(0, |m| m + 1);
        let strip_w = if total_slots == 0 {
            0
        } else {
            (total_slots * 4 - 1) as u16 // n buttons * 3 + (n - 1) gaps
        };
        let start =
            self.terminal_area.x + (self.terminal_area.width.saturating_sub(strip_w)) / 2;
        let top_id = self.slots.last().map(|s| s.id);
        for (i, slot) in self.slots.iter().enumerate() {
            // hide_when_empty: no button (and nothing clickable) while the
            // layer owns no widgets — its slot stays reserved.
            let hidden = slot.layer.hide_when_empty()
                && self.cached_widgets.get(&slot.id).is_none_or(|ws| ws.is_empty());
            if hidden {
                continue;
            }
            let s = self.slot_of.get(&slot.id).copied().unwrap_or(i) as u16;
            let area = Rect::new(start + s * 4, row, 3, 1);
            let color = if slot.id == self.builtin_id {
                Color::DarkGray
            } else {
                self.layer_color(slot.id).unwrap_or(Color::DarkGray)
            };
            let mut style = Style::default().bg(color).fg(Color::Black);
            if Some(slot.id) == top_id {
                style = style.add_modifier(Modifier::BOLD);
            }
            widgets.push(WidgetEntry {
                name: "display.taskbar",
                widget: Box::new(Paragraph::new(Span::styled(
                    format!("{:^3}", slot.label),
                    style,
                ))),
                area,
            });
            self.taskbar.push((slot.id, area));
        }
    }

    /// Emit one backdrop rect per widget of the group (all before the
    /// group's first widget). User layers clear and paint their color; the
    /// builtin group clears to the terminal default — necessary now that
    /// the builtin can rise above other layers (its widgets don't paint
    /// every cell, so lower layers would bleed through otherwise).
    fn flush_group(
        &self,
        decorated: &mut Vec<WidgetEntry>,
        group: &mut Vec<WidgetEntry>,
        owner: Option<LayerId>,
    ) {
        if let Some(owner) = owner {
            let backdrop = if owner == self.builtin_id {
                Some(Color::Reset)
            } else {
                self.layer_color(owner)
            };
            if let Some(color) = backdrop {
                for w in group.iter() {
                    decorated.push(WidgetEntry {
                        name: "display.backdrop",
                        widget: Box::new(LayerBackdrop { color }),
                        area: w.area,
                    });
                }
            }
        }
        decorated.append(group);
    }

    /// Route one terminal event. Returns `true` to swallow it (servatui's
    /// builtin handling is then skipped), `false` to let it fall through.
    /// Requires a previous [`Display::frame`] this session (routing uses
    /// the stack order and widget areas of the last frame).
    ///
    /// # Focus semantics
    ///
    /// Activation implements a kind of focus: the active (topmost) layer
    /// is offered key input first, and **any input swallowed by the
    /// focused layer is not given to any other layer** (nor to the builtin
    /// handling) — a swallow is consumption. Passed keys fall through to
    /// lower layers and finally the core; when the builtin layer itself is
    /// focused, keys go straight to the core's input line. Mouse events
    /// are spatial instead: the visual occupant of the click point
    /// receives them exclusively. See the crate docs for the full model.
    pub fn route_event(&mut self, ev: &Event) -> bool {
        use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};

        // Shift+Tab is system-reserved: rotate activation one step down the
        // stack (wrapping around), through all layers including the builtin.
        if let Event::Key(k) = ev {
            if k.kind == KeyEventKind::Press
                && (k.code == KeyCode::Tab && k.modifiers.contains(KeyModifiers::SHIFT)
                    || k.code == KeyCode::BackTab)
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

        // Mouse events: the topmost layer whose area contains the click is
        // the VISUAL OCCUPANT of that point — it gets the event and lower
        // layers never see it. If it swallows, a press also activates +
        // grabs it; if it passes, a press still focuses it (click-to-focus)
        // and the event falls through to the core — which is how clicking
        // the log/input activates the builtin layer.
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
                if is_down {
                    self.activate(id);
                }
                return false;
            }
            return false;
        }

        // Keys / resize / focus: offered to every layer, topmost first.
        // The builtin layer is no special case — when it is topmost (or the
        // only layer), its own on_event handles keystrokes through the
        // shared BuiltinTui and swallows them.
        for i in (0..self.slots.len()).rev() {
            if self.deliver(self.slots[i].id, ev) == EventResult::Swallow {
                return true;
            }
        }
        false
    }

    /// Run this display as the TUI client: creates the shared
    /// [`BuiltinTui`], attaches it to the builtin layer (so the builtin
    /// becomes a completely ordinary layer — its events are handled inside
    /// `on_event`, with no router special cases), and drives the loop.
    pub fn run(
        self,
        socket: &'a std::path::Path,
        protocols: &'a [servyi_servatui::Protocol],
    ) -> Result<(), String> {
        let builtin = Rc::new(RefCell::new(servyi_servatui::tui::BuiltinTui::new(
            socket, protocols,
        )));
        let mut display = self;
        display.attach_builtin(builtin.clone());
        let display = RefCell::new(display);
        servyi_servatui::tui::run_tui_managed(
            builtin,
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
