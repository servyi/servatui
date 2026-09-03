# AGENTS.md

## Build & Test

```sh
cargo build
cargo test                          # integration tests (real Unix sockets)
cargo clippy                        # zero warnings required
```

## Architecture

servatui is a generic server-client framework with TUI/CLI frontends.
It knows nothing about any specific domain (no FUSE, no secrets, no containers).

### Core design: Type-state builder chain

```
Plugin::new("cmd", "help")
    .parse(|&str| → T0)           // runs on CLIENT
    → Client<T0>
    .client(|T0, Console, Input| → T1)  // CLIENT step: process T0, send T1 to server
    → Server<T1>
    .server(|T1| → T2)            // SERVER step: recv T1, process, send T2 back
    → Client<T2>
    .client(|T2, Console, Input| → T3)  // CLIENT step: recv T2, process
    → Server<T3>
    .finalize(|| → ShellAction)    // terminal
    → Protocol
```

The type alternates Client → Server → Client → Server → ... — enforced by
the Rust compiler. You cannot call .client() on Server or .server() on Client.

### Wire protocol (alternating C→S, S→C)

```
C→S: output of client step 1
S→C: output of server step 1
C→S: output of client step 2
S→C: output of server step 2
...
C→S: sentinel ()
```

### Key types

- `RawConnection` (object-safe): send_bytes/recv_bytes
- `TypedConnection` (blanket impl): send_typed<T>/recv_typed<T>
- `Console`: print_line/print_error (CLI: stdout, TUI: log area)
- `InputSource`: read_line (CLI: stdin, TUI: modal prompt)
- `ErasedStep` (private): type-erased step with client_exec/server_exec
- `Protocol`: complete chain + metadata, has run_client/run_server
- `App`: holds protocols + socket path, has run_server/run_cli_command

### Versioning

Increment the minor version for every build. Shared between client and server.
The version is part of the protocol — client detects mismatch and can restart server.

### TUI Mouse Selection Testing

The TUI uses mouse capture for text selection, scrollbar dragging, and scroll wheel.
Changes to layout or mouse handling MUST be verified with both automated tests
and the interactive mouse recorder.

**Automated tests** (`src/tui.rs` `#[cfg(test)] mod tests`):
- `test_leftward_drag_includes_anchor_char` — leftward drag must include both endpoints
- `test_auto_scroll_up_on_upward_drag` — dragging above viewport scrolls immediately
- `test_auto_scroll_down_on_downward_drag` — dragging below viewport scrolls immediately
- `test_log_inner_area_excludes_borders` — content area must not overlap borders
- `test_extract_multiline_no_continuation_newlines` — wrapped lines join without \n
- `test_word_boundaries_*` — double-click word selection boundaries

Run: `cargo test --features tui -- tui::tests`

**Interactive mouse recorder** (`examples/mouse-recorder.rs`):
```sh
cargo run --features tui --example mouse-recorder 2>&1 | tee /tmp/recorder.log
```
Guides through click, drag, double-click, scroll, scrollbar, ctrl+drag, right-click,
middle-click. Highlights mouse position in real-time. Dumps all raw events + Rust
test replay code on exit.

**When to use the recorder:**
- After changing the layout (border sizes, widget positions)
- After changing mouse event routing logic
- When debugging scrollbar or selection issues
- When verifying behavior on a new terminal emulator

**Process for adding a new test (test-driven):**
1. Write the test in `src/tui.rs` `mod tests` BEFORE writing the fix
2. Commit: "add failing test for bug X" (CI shows red)
3. Write the fix
4. Commit: "fix bug X" (CI shows green)

The test must fail against the current code first. If it passes, the test
is wrong or the bug doesn't exist. Fix the test or investigate before proceeding.

## Display Layers (`display/` crate)

`servatui-display` is a window-manager layer stack built on the closure API
(core stays untouched except for the non-breaking `run_tui_with_events`
event hook). A `Display` owns `Vec<DisplayLayer>`; each layer has a unique
continuous id mapped onto a color palette (≤8 terminal colors: all of them;
≥16: the 8 darker shades).

- **Frame** (`Display::frame`): sort by priority, collapse priorities to
  adjacent integers, run `on_overlay` in order (later = on top), attribute
  new widget names to the adding layer, regroup the vec so each layer's
  widgets are consecutive in stack order (activating builtin brings
  log/input to the front), insert one CLEARING backdrop rect per widget
  before each non-builtin group (never interleaved with the widgets), and
  append the bottom-row taskbar: 3-wide buttons, 1 space apart, centered,
  at stable id-assigned slots recycled on removal.
- **Routing** (`Display::route_event`): Shift+Tab is system-reserved
  (carousel rotation: each press raises the bottom-most layer); mouse
  events stop at the visual occupant (the topmost layer whose area
  contains the point gets the event; lower layers never see it — a
  covered popup is NOT raised by clicks, only via taskbar/Shift+Tab);
  a passed press still activates the occupant (click-to-focus) and falls
  through to the core; a swallowed press also grabs the pointer
  (drags/release follow the grabber outside its areas);
  keys/resize/focus are offered topmost-first with fall-through — unless
  the builtin layer is topmost, in which case they fall straight through
  to the core's input line so layers below cannot intercept; everything
  passed falls through to the builtin handling.
- **Layer lifecycle extras**: `on_overlay` returns a StackIntent
  (Top/Bottom/Keep) applied to the next frame's ordering; `on_active`
  fires when activate() targets the layer (taskbar click, Shift+Tab,
  swallowed press, click-to-focus, Top intent); `hide_when_empty()`
  layers lose their taskbar button while they own no widgets but keep
  their reserved slot.

**When changing routing or attribution logic:**
1. Write the test in `display/tests/display.rs` BEFORE the fix (must fail)
2. Commit "add failing test for bug X", then the fix
3. `cargo test --workspace`, `cargo clippy --all-targets` (zero warnings)
4. Re-run the `display` fuzz target (`fuzz/`, nightly): the invariants are
   priorities-adjacent-after-collapse, one-taskbar-button-per-layer, grab
   cleared on Up, grab holder is a live layer

**Examples** (`cargo run -p servatui-display --example …`): `popup`
(clickable modal), `picker` (↑/↓/Enter/Esc list), `textbox` (modal input).
