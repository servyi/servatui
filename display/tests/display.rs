//! Headless tests for the display layer stack: routing, priorities,
//! attribution, underlays, taskbar, grab, and rotation.

use std::path::PathBuf;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use servatui_display::{detect_color_count, palette_for, Display, DisplayLayer, EventResult, LayerCtx, LayerId};
use servyi_servatui::{WidgetEntry, WIDGET_INPUT, WIDGET_LOG};

// ── helpers ───────────────────────────────────────────────────────────

fn key(code: KeyCode, mods: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, mods))
}

fn plain(code: KeyCode) -> Event {
    key(code, KeyModifiers::NONE)
}

fn down(col: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: col, row, modifiers: KeyModifiers::NONE })
}

fn drag(col: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Drag(MouseButton::Left), column: col, row, modifiers: KeyModifiers::NONE })
}

fn up(col: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), column: col, row, modifiers: KeyModifiers::NONE })
}

fn wheel(col: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind: MouseEventKind::ScrollUp, column: col, row, modifiers: KeyModifiers::NONE })
}

fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
    Rect::new(x, y, w, h)
}

/// A widget entry whose render is a plain block (enough for attribution).
fn widget(name: &'static str, area: Rect) -> WidgetEntry {
    WidgetEntry { name, widget: Box::new(Paragraph::new("x")), area }
}

/// The builtin frame the core would hand us: log + input tiling 80x24.
fn builtin_frame() -> Vec<WidgetEntry> {
    vec![
        widget(WIDGET_LOG, rect(0, 0, 80, 21)),
        widget(WIDGET_INPUT, rect(0, 21, 80, 3)),
    ]
}

/// Layer with a fixed widget; records every event it was offered.
struct Toy {
    widget_name: &'static str,
    area: Rect,
    keys: Vec<KeyCode>,
    swallow_mouse: bool,
    log: Vec<String>,
}

impl Toy {
    fn new(widget_name: &'static str, area: Rect) -> Self {
        Self { widget_name, area, keys: Vec::new(), swallow_mouse: false, log: Vec::new() }
    }

    fn swallow_keys(mut self, keys: &[KeyCode]) -> Self {
        self.keys = keys.to_vec();
        self
    }

    fn swallow_mouse(mut self) -> Self {
        self.swallow_mouse = true;
        self
    }
}

impl DisplayLayer for Toy {
    fn on_overlay(&mut self, _ctx: &mut LayerCtx, widgets: &mut Vec<WidgetEntry>) {
        widgets.push(widget(self.widget_name, self.area));
    }

    fn on_event(&mut self, ev: &Event, _ctx: &LayerCtx) -> EventResult {
        self.log.push(format!("{ev:?}"));
        match ev {
            Event::Key(k) if k.kind == KeyEventKind::Press && self.keys.contains(&k.code) => {
                EventResult::Swallow
            }
            // Mouse events are only offered when the pointer is inside our
            // areas (or we hold the grab), so swallow all of them.
            Event::Mouse(_) if self.swallow_mouse => EventResult::Swallow,
            _ => EventResult::Pass,
        }
    }
}

fn display_with(toys: Vec<Toy>) -> (Display, Vec<LayerId>) {
    let mut display = Display::with_palette(palette_for(16));
    let ids = toys
        .into_iter()
        .map(|t| display.add_layer(Box::new(t)))
        .collect();
    (display, ids)
}

// ── palette ───────────────────────────────────────────────────────────

#[test]
fn palette_uses_all_colors_when_8_or_fewer() {
    let p8 = palette_for(8);
    assert_eq!(p8.len(), 8);
    assert!(p8.contains(&ratatui::style::Color::Gray));
    assert!(!p8.contains(&ratatui::style::Color::DarkGray));

    let p16 = palette_for(16);
    assert_eq!(p16.len(), 8);
    assert!(p16.contains(&ratatui::style::Color::DarkGray));
    assert!(!p16.contains(&ratatui::style::Color::Gray));
}

#[test]
fn colors_map_onto_continuous_layer_ids() {
    let palette = palette_for(16);
    let mut display = Display::with_palette(palette.clone());
    for i in 0..(palette.len() * 2) {
        let id = display.add_layer(Box::new(Toy::new("x", rect(0, 0, 1, 1))));
        assert_eq!(display.layer_color(id), Some(palette[i % palette.len()]));
    }
    // builtin has no palette color
    let (display, _) = display_with(vec![]);
    assert_eq!(display.owner_of(WIDGET_LOG), None);
}

#[test]
fn detect_color_count_is_sane() {
    let n = detect_color_count();
    assert!(n >= 8, "fallback must be at least 8, got {n}");
}

// ── frame: ordering, collapse, attribution, underlay, taskbar ─────────

#[test]
fn activation_raises_and_collapse_keeps_priorities_adjacent() {
    let (mut display, ids) = display_with(vec![
        Toy::new("a", rect(0, 0, 5, 5)),
        Toy::new("b", rect(0, 6, 5, 5)),
        Toy::new("c", rect(0, 12, 5, 5)),
    ]);
    let a = ids[0];
    let mut frame = builtin_frame();
    display.frame(&mut frame);
    assert_eq!(display.stack_order(), vec![LayerId::BUILTIN, ids[0], ids[1], ids[2]]);

    // Activating the bottom layer puts it on top.
    display.activate(a);
    display.frame(&mut frame);
    assert_eq!(display.stack_order(), vec![LayerId::BUILTIN, ids[1], ids[2], ids[0]]);

    // Repeated activation must not inflate the priority space: after many
    // rounds the stack is still the same 4 layers in rotation order.
    for expected_top in [ids[1], ids[2], ids[0]] {
        display.activate(expected_top);
        display.frame(&mut frame);
        assert_eq!(display.topmost(), Some(expected_top));
    }
}

#[test]
fn layer_widgets_get_colored_underlay_builtins_do_not() {
    let (mut display, ids) = display_with(vec![Toy::new("popup", rect(10, 5, 20, 10))]);
    let mut frame = builtin_frame();
    display.frame(&mut frame);

    let owner = display.owner_of("popup").expect("popup attributed");
    assert_eq!(owner, ids[0]);
    assert_eq!(display.owner_of(WIDGET_LOG), Some(LayerId::BUILTIN));

    // A backdrop entry is injected directly before the popup's group, but
    // the builtin widgets get none (WIDGET_LOG is first in the vec, and no
    // backdrop anywhere carries its area).
    let names: Vec<&'static str> = frame.iter().map(|w| w.name).collect();
    let popup_idx = names.iter().position(|n| *n == "popup").unwrap();
    assert_eq!(frame[popup_idx - 1].name, "display.backdrop");
    assert_eq!(frame[popup_idx - 1].area, rect(10, 5, 20, 10));
    assert_eq!(frame[0].name, WIDGET_LOG);
    assert!(!frame.iter().any(|w| {
        w.name == "display.backdrop" && w.area == rect(0, 0, 80, 21)
    }), "builtin log must not get a backdrop");
}

#[test]
fn taskbar_one_cell_per_layer_on_bottom_row_click_activates() {
    let (mut display, ids) = display_with(vec![
        Toy::new("a", rect(0, 0, 5, 5)),
        Toy::new("b", rect(0, 6, 5, 5)),
    ]);
    let mut frame = builtin_frame();
    display.frame(&mut frame);

    // Bottom row of the derived 80x24 terminal area; the centered strip
    // (3-wide buttons, 1 gap) starts at (80 - 11) / 2 = 34:
    // builtin(slot 0)@34, a(slot 1)@38, b(slot 2)@42.
    let cells: Vec<(&'static str, Rect)> = frame
        .iter()
        .filter(|w| w.name == "display.taskbar")
        .map(|w| (w.name, w.area))
        .collect();
    assert_eq!(cells.len(), 3);
    assert!(cells.iter().all(|(_, a)| a.y == 23 && a.height == 1 && a.width == 3));

    // Clicking a's cell (slot 1: x 38..=40) activates a.
    assert!(display.route_event(&down(39, 23)));
    assert_eq!(display.topmost(), Some(ids[0]));

    // Clicking the builtin cell (slot 0: x 34..=36) activates builtin.
    assert!(display.route_event(&down(35, 23)));
    assert_eq!(display.topmost(), Some(LayerId::BUILTIN));
}

#[test]
fn terminal_area_derived_from_input_widget() {
    let (mut display, _) = display_with(vec![Toy::new("a", rect(0, 0, 5, 5))]);
    let mut frame = builtin_frame();
    display.frame(&mut frame);
    // ctx exposed the derived area during on_overlay; check indirectly via
    // the taskbar row (bottom of an 80x24 area = row 23).
    let taskbar_y = frame.iter().rev().find(|w| w.name == "display.taskbar").unwrap().area.y;
    assert_eq!(taskbar_y, 23);
}

// ── routing ───────────────────────────────────────────────────────────

#[test]
fn key_events_fall_through_the_stack_topmost_first() {
    let (mut display, ids) = display_with(vec![
        Toy::new("top", rect(0, 0, 80, 24)).swallow_keys(&[KeyCode::Char('y')]),
        Toy::new("bottom", rect(0, 0, 80, 24)).swallow_keys(&[KeyCode::Char('x')]),
    ]);
    let mut frame = builtin_frame();
    display.frame(&mut frame);

    // Top swallows 'y'.
    assert!(display.route_event(&plain(KeyCode::Char('y'))));
    // Top passes 'x', bottom swallows it.
    assert!(display.route_event(&plain(KeyCode::Char('x'))));
    // Nobody swallows 'z' — falls through to the builtin handling.
    assert!(!display.route_event(&plain(KeyCode::Char('z'))));
    let _ = ids;
}

#[test]
fn swallowed_press_activates_and_grabs_drag_outside_area() {
    let (mut display, ids) = display_with(vec![Toy::new("pop", rect(10, 5, 10, 5)).swallow_mouse()]);
    let mut frame = builtin_frame();
    display.frame(&mut frame);

    // Press inside the popup: swallowed, activated, grabbed.
    assert!(display.route_event(&down(12, 7)));
    assert_eq!(display.topmost(), Some(ids[0]));
    assert_eq!(display.grabbed(), Some(ids[0]));

    // Drag far outside the popup: still delivered to the grabber.
    assert!(display.route_event(&drag(70, 20)));

    // Release clears the grab.
    assert!(display.route_event(&up(70, 20)));
    assert_eq!(display.grabbed(), None);
}

#[test]
fn wheel_over_layer_never_activates() {
    let (mut display, ids) = display_with(vec![Toy::new("pop", rect(10, 5, 10, 5)).swallow_mouse()]);
    let mut frame = builtin_frame();
    display.frame(&mut frame);

    let before = display.topmost();
    assert!(display.route_event(&wheel(12, 7)));
    assert_eq!(display.topmost(), before, "wheel must not change activation");
    assert_eq!(display.grabbed(), None);
    let _ = ids;
}

#[test]
fn click_outside_all_layers_falls_through() {
    let (mut display, _) = display_with(vec![Toy::new("pop", rect(10, 5, 10, 5)).swallow_mouse()]);
    let mut frame = builtin_frame();
    display.frame(&mut frame);

    assert!(!display.route_event(&down(60, 15)));
    assert_eq!(display.grabbed(), None);
}

#[test]
fn shift_tab_rotates_through_all_layers_including_builtin() {
    let (mut display, ids) = display_with(vec![
        Toy::new("a", rect(0, 0, 5, 5)),
        Toy::new("b", rect(0, 6, 5, 5)),
        Toy::new("c", rect(0, 12, 5, 5)),
    ]);
    let mut frame = builtin_frame();
    display.frame(&mut frame);
    assert_eq!(display.topmost(), Some(ids[2]));

    let shift_tab = key(KeyCode::Tab, KeyModifiers::SHIFT);
    // Carousel: each press raises the bottom-most layer. From [builtin, a,
    // b, c] (topmost c) the cycle is builtin -> a -> b -> c -> ...
    assert!(display.route_event(&shift_tab));
    assert_eq!(display.topmost(), Some(LayerId::BUILTIN));
    assert!(display.route_event(&shift_tab));
    assert_eq!(display.topmost(), Some(ids[0]));
    assert!(display.route_event(&shift_tab));
    assert_eq!(display.topmost(), Some(ids[1]));
    assert!(display.route_event(&shift_tab));
    assert_eq!(display.topmost(), Some(ids[2]));

    // Plain Tab is NOT rotation (it belongs to the input line).
    assert!(!display.route_event(&plain(KeyCode::Tab)));
}

#[test]
fn removing_a_layer_clears_ownership_and_grab() {
    let (mut display, ids) = display_with(vec![Toy::new("pop", rect(10, 5, 10, 5)).swallow_mouse()]);
    let mut frame = builtin_frame();
    display.frame(&mut frame);

    assert!(display.route_event(&down(12, 7)));
    assert_eq!(display.grabbed(), Some(ids[0]));

    display.remove_layer(ids[0]).expect("layer removed");
    assert_eq!(display.grabbed(), None);
    assert_eq!(display.owner_of("pop"), None);
    // Its events now fall through.
    assert!(!display.route_event(&down(12, 7)));
}

#[test]
fn new_layers_start_on_top() {
    let (mut display, ids) = display_with(vec![
        Toy::new("a", rect(0, 0, 5, 5)),
        Toy::new("b", rect(0, 6, 5, 5)),
    ]);
    let mut frame = builtin_frame();
    display.frame(&mut frame);
    assert_eq!(display.topmost(), Some(ids[1]));
}

// ── rendering smoke: underlay actually paints ─────────────────────────

/// A widget that fills its whole area with one character — like the log
/// widget writes text everywhere, so we can observe what a backdrop clears.
struct Fill(char);

impl ratatui::widgets::WidgetRef for Fill {
    fn render_ref(&self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(self.0);
                }
            }
        }
    }
}

struct FillLayer {
    name: &'static str,
    area: Rect,
}

impl DisplayLayer for FillLayer {
    fn on_overlay(&mut self, _ctx: &mut LayerCtx, widgets: &mut Vec<WidgetEntry>) {
        widgets.push(WidgetEntry {
            name: self.name,
            widget: Box::new(Fill('A')),
            area: self.area,
        });
    }
}

struct ParaLayer {
    area: Rect,
    second: Rect,
}

impl DisplayLayer for ParaLayer {
    fn on_overlay(&mut self, _ctx: &mut LayerCtx, widgets: &mut Vec<WidgetEntry>) {
        widgets.push(WidgetEntry {
            name: "b.para",
            widget: Box::new(Paragraph::new("B")),
            area: self.area,
        });
        widgets.push(WidgetEntry {
            name: "b.second",
            widget: Box::new(Paragraph::new("2")),
            area: self.second,
        });
    }
}

#[test]
fn backdrop_clears_content_and_is_not_interleaved_with_widgets() {
    let mut display = Display::with_palette(palette_for(16));
    display.add_layer(Box::new(FillLayer { name: "a.fill", area: rect(0, 0, 10, 3) }));
    let b_id = display.add_layer(Box::new(ParaLayer {
        area: rect(5, 0, 10, 3),
        second: rect(20, 0, 6, 3),
    }));
    let b_color = display.layer_color(b_id).unwrap();

    let mut frame = builtin_frame();
    display.frame(&mut frame);

    let names: Vec<&'static str> = frame.iter().map(|w| w.name).collect();
    // B's two backdrops sit directly before its first widget — never
    // interleaved with the widgets themselves.
    let para_idx = names.iter().position(|n| *n == "b.para").unwrap();
    assert_eq!(names[para_idx - 1], "display.backdrop");
    assert_eq!(names[para_idx - 2], "display.backdrop");
    // One backdrop rect per widget (not a bounding rect over both).
    assert_eq!(frame[para_idx - 2].area, rect(5, 0, 10, 3));
    assert_eq!(frame[para_idx - 1].area, rect(20, 0, 6, 3));

    // Render: the bottom layer's 'A's must not bleed through B's area.
    let mut buf = ratatui::buffer::Buffer::empty(rect(0, 0, 80, 24));
    for e in &frame {
        e.widget.render_ref(e.area, &mut buf);
    }
    assert_eq!(buf[(2, 1)].symbol(), "A", "bottom layer visible outside B");
    assert_eq!(buf[(5, 0)].symbol(), "B", "top widget content drawn");
    assert_eq!(buf[(8, 1)].symbol(), " ", "backdrop must clear the A below");
    assert_eq!(buf[(8, 1)].bg, b_color);
    // The gap between B's two widgets belongs to no backdrop rect.
    assert_ne!(buf[(16, 1)].bg, b_color);
}

#[test]
fn activating_builtin_brings_its_widgets_to_front() {
    let (mut display, _ids) = display_with(vec![Toy::new("a", rect(0, 0, 5, 5))]);
    let mut frame = builtin_frame();
    display.frame(&mut frame);
    assert_eq!(frame[0].name, WIDGET_LOG, "builtin starts at the bottom");

    display.activate(LayerId::BUILTIN);
    let mut frame = builtin_frame();
    display.frame(&mut frame);
    let names: Vec<&'static str> = frame.iter().map(|w| w.name).collect();
    // The taskbar strip (one button per layer) is appended after everything;
    // the builtin group itself must be the last widget group, drawn on top.
    let n = names.len();
    assert_eq!(names[n - 3], WIDGET_INPUT, "activated builtin draws on top");
    assert_eq!(names[n - 4], WIDGET_LOG);
    assert_eq!(names[n - 1], "display.taskbar");
    assert_eq!(names[n - 2], "display.taskbar");
}

#[test]
fn builtin_topmost_directs_keys_to_the_core() {
    let (mut display, ids) = display_with(vec![
        Toy::new("a", rect(0, 0, 80, 24)).swallow_keys(&[KeyCode::Char('x')]),
    ]);
    let mut frame = builtin_frame();
    display.frame(&mut frame);
    // User layer on top: it intercepts 'x'.
    assert!(display.route_event(&plain(KeyCode::Char('x'))));

    // Builtin on top: keystrokes fall through to the core's input line,
    // not to layers below it.
    display.activate(LayerId::BUILTIN);
    let mut frame = builtin_frame();
    display.frame(&mut frame);
    assert!(!display.route_event(&plain(KeyCode::Char('x'))));

    // Raising the user layer back restores interception.
    display.activate(ids[0]);
    assert!(display.route_event(&plain(KeyCode::Char('x'))));
}

#[test]
fn taskbar_slots_are_stable_centered_and_three_wide() {
    let (mut display, ids) = display_with(vec![
        Toy::new("a", rect(0, 0, 5, 5)),
        Toy::new("b", rect(0, 6, 5, 5)),
    ]);
    let cells = |display: &mut Display| {
        let mut frame = builtin_frame();
        display.frame(&mut frame);
        let mut cells: Vec<Rect> =
            frame.iter().filter(|w| w.name == "display.taskbar").map(|w| w.area).collect();
        cells.sort_by_key(|r| r.x);
        cells
    };

    let c = cells(&mut display);
    assert_eq!(c.len(), 3);
    // 3-wide buttons with a 1-column gap, strip centered on the 80-wide row.
    let start = (80 - (3 * 4 - 1)) / 2;
    for (i, cell) in c.iter().enumerate() {
        assert_eq!(cell.width, 3, "buttons are 3 cells wide");
        assert_eq!(cell.x, start + i as u16 * 4, "1 space between buttons");
        assert_eq!(cell.y, 23);
    }

    // Stable under activation: raising `a` must not move anyone's cell.
    display.activate(ids[0]);
    assert_eq!(cells(&mut display), c, "positions must not follow priority order");

    // Slot recycling: removing b frees its slot; a new layer reuses it.
    let b_cell = c[2];
    display.remove_layer(ids[1]);
    assert_eq!(cells(&mut display).len(), 2);
    display.add_layer(Box::new(Toy::new("c", rect(0, 12, 5, 5))));
    let c3 = cells(&mut display);
    assert_eq!(c3.len(), 3);
    assert!(c3.contains(&b_cell), "a new layer must recycle b's freed slot");
}

#[test]
fn underlay_paints_layer_color_behind_widget() {
    let (mut display, ids) = display_with(vec![Toy::new("pop", rect(10, 5, 10, 10)).swallow_mouse()]);
    let mut frame = builtin_frame();
    display.frame(&mut frame);

    let color = display.layer_color(ids[0]).unwrap();
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| {
        for entry in &frame {
            entry.widget.render_ref(entry.area, f.buffer_mut());
        }
    }).unwrap();

    // A cell inside the popup area carries the layer's background.
    let cell = terminal.backend().buffer()[(15u16, 10u16)].clone();
    assert_eq!(cell.bg, color, "popup area must carry the layer color underlay");
    let _ = PathBuf::new();
}
