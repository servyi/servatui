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
