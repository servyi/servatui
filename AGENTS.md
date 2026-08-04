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

**Process for adding a new test:**
1. Write the test in `src/tui.rs` `mod tests`
2. `git stash` your changes
3. `git checkout -b verify-tests HEAD~1` (or the commit before your fix)
4. Apply only the test code, run it — it MUST fail (proves the bug exists)
5. `git checkout main && git stash pop`
6. Run the test — it MUST pass (proves the fix works)
7. `git branch -D verify-tests`
