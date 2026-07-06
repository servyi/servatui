//! Integration test: start a real server, connect a client, verify response.

use servyi_servatui::*;
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize)]
struct EchoArgs { text: String }

#[derive(Serialize, Deserialize)]
struct EchoResult { text: String }

fn make_echo_protocol() -> Protocol {
    Plugin::new("echo", "Echo text back")
        .parse(|args: &str| Ok(EchoArgs { text: args.to_string() }))
        // Client: passthrough (send args to server)
        .client(|args: EchoArgs, _out, _input| Ok(args))
        // Server: echo
        .server(|args: EchoArgs| Ok(EchoResult { text: args.text }))
        // Client: render
        .client(|result: EchoResult, out, _input| {
            out.print_line(&result.text);
            Ok(())
        })
        .finalize(|| Ok(ShellAction::Continue))
}

#[derive(Serialize, Deserialize)]
struct PingResult { message: String }

fn make_ping_protocol() -> Protocol {
    Plugin::new("ping", "Ping the server")
        .parse(|_args: &str| Ok(()))
        // Client: send empty
        .client(|_: (), _out, _input| Ok(()))
        // Server: respond
        .server(|_: ()| Ok(PingResult { message: "pong".into() }))
        // Client: render
        .client(|result: PingResult, out, _input| {
            out.print_line(&result.message);
            Ok(())
        })
        .finalize(|| Ok(ShellAction::Continue))
}

#[test]
fn test_echo_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("test.sock");

    let app = App::builder(&socket)
        .protocol(make_echo_protocol())
        .protocol(make_ping_protocol())
        .build();

    // Start server in background thread
    let server_socket = socket.clone();
    let server_protocols = vec![make_echo_protocol(), make_ping_protocol()];
    let server_handle = ServerHandle {
        socket: server_socket,
        protocols: server_protocols,
    };
    let _server_thread = std::thread::spawn(move || {
        server_handle.run(&())
    });

    // Wait for server to start
    for _ in 0..100 {
        if app.server_running() { break; }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(app.server_running(), "Server did not start");

    // Run echo command
    let lines = app.run_cli_command("echo", "hello world").unwrap();
    assert_eq!(lines, vec!["hello world"]);

    // Run ping command
    let lines = app.run_cli_command("ping", "").unwrap();
    assert_eq!(lines, vec!["pong"]);

    // Clean up
    drop(app);
    let _ = std::fs::remove_file(&socket);
}

#[test]
fn test_unknown_command() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("test2.sock");

    let app = App::builder(&socket)
        .protocol(make_echo_protocol())
        .build();

    let server_protocols = vec![make_echo_protocol()];
    let server_handle = ServerHandle {
        socket: socket.clone(),
        protocols: server_protocols,
    };
    let _server_thread = std::thread::spawn(move || {
        server_handle.run(&())
    });

    for _ in 0..100 {
        if app.server_running() { break; }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let result = app.run_cli_command("nonexistent", "");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("Unknown command"), "got: {err}");

    let _ = std::fs::remove_file(&socket);
}

#[test]
fn test_commands_listing() {
    let app = App::builder("/tmp/dummy.sock")
        .protocol(make_echo_protocol())
        .protocol(make_ping_protocol())
        .build();

    let cmds = app.commands();
    assert_eq!(cmds.len(), 2);
    assert!(cmds.iter().any(|(n, _)| *n == "echo"));
    assert!(cmds.iter().any(|(n, _)| *n == "ping"));
}
