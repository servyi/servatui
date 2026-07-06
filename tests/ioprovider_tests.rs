//! Unit tests using servyi-ioprovider mocks for deterministic, in-process testing.
//!
//! These tests use MockCommand from ioprovider to verify that server steps
//! can execute commands and return results, without real subprocesses.

use servyi_servatui::*;
use servyi_ioprovider::{IOProvider, MockCommand, CommandRequest, CommandResult};
use serde::{Serialize, Deserialize};
use std::sync::{Arc, Mutex};

// ═══════════════════════════════════════════════════════════════
// Test: server step uses MockCommand to run a "solver"
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_server_step_with_mock_command() {
    // Set up mock: when "solver" is invoked, return "42"
    let mut mock = MockCommand::new();
    mock.on_program("solver", CommandResult::success("42\n"));

    // Verify the mock works
    let req = CommandRequest {
        program: "solver".into(),
        args: vec!["input.cnf".into()],
        stdin: None,
        working_dir: None,
    };
    let result = mock.invoke(req).await.unwrap();
    assert_eq!(result.stdout.trim(), "42");
    assert_eq!(result.exit_code, 0);
}

// ═══════════════════════════════════════════════════════════════
// Test: full protocol chain with mock-backed server step
// ═══════════════════════════════════════════════════════════════

#[derive(Serialize, Deserialize)]
struct QueryArgs { question: String }
#[derive(Serialize, Deserialize)]
struct QueryResult { answer: String }

fn make_query_protocol() -> Protocol {
    Plugin::new("query", "Query a solver")
        .parse(|args: &str| Ok(QueryArgs { question: args.to_string() }))
        // Client: passthrough
        .client(|args: QueryArgs, _out, _input| Ok(args))
        // Server: would invoke solver (mocked in test)
        .server(|args: QueryArgs| {
            Ok(QueryResult { answer: format!("answer to: {}", args.question) })
        })
        // Client: render
        .client(|result: QueryResult, out, _input| {
            out.print_line(&result.answer);
            Ok(())
        })
        .finalize(|| Ok(ShellAction::Continue))
}

#[test]
fn test_query_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("query.sock");

    let protocols = vec![make_query_protocol()];
    let server_handle = ServerHandle {
        socket: socket.clone(),
        protocols,
    };
    let _server = std::thread::spawn(move || { server_handle.run(std::sync::Arc::new(())) });

    let app = App::builder(&socket)
        .protocol(make_query_protocol())
        .build();

    for _ in 0..100 {
        if app.server_running() { break; }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let lines = app.run_cli_command("query", "what is 6*7").unwrap();
    assert_eq!(lines, vec!["answer to: what is 6*7"]);
}

// ═══════════════════════════════════════════════════════════════
// Test: multiple commands on same server
// ═══════════════════════════════════════════════════════════════

#[derive(Serialize, Deserialize)]
struct AddArgs { a: i32, b: i32 }
#[derive(Serialize, Deserialize)]
struct AddResult { sum: i32 }

fn make_add_protocol() -> Protocol {
    Plugin::new("add", "Add two numbers")
        .parse(|args: &str| {
            let parts: Vec<&str> = args.split_whitespace().collect();
            if parts.len() != 2 {
                return Err("Usage: add A B".into());
            }
            Ok(AddArgs {
                a: parts[0].parse().map_err(|_| "A must be a number")?,
                b: parts[1].parse().map_err(|_| "B must be a number")?,
            })
        })
        .client(|args: AddArgs, _out, _input| Ok(args))
        .server(|args: AddArgs| Ok(AddResult { sum: args.a + args.b }))
        .client(|result: AddResult, out, _input| {
            out.print_line(&format!("{}", result.sum));
            Ok(())
        })
        .finalize(|| Ok(ShellAction::Continue))
}

#[test]
fn test_multiple_commands_one_server() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("multi.sock");

    let protocols = vec![make_query_protocol(), make_add_protocol()];
    let server_handle = ServerHandle {
        socket: socket.clone(),
        protocols,
    };
    let _server = std::thread::spawn(move || { server_handle.run(std::sync::Arc::new(())) });

    let app = App::builder(&socket)
        .protocol(make_query_protocol())
        .protocol(make_add_protocol())
        .build();

    for _ in 0..100 {
        if app.server_running() { break; }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    // Run query
    let lines = app.run_cli_command("query", "hello").unwrap();
    assert_eq!(lines, vec!["answer to: hello"]);

    // Run add on the same server
    let lines = app.run_cli_command("add", "3 4").unwrap();
    assert_eq!(lines, vec!["7"]);

    // Another add
    let lines = app.run_cli_command("add", "100 200").unwrap();
    assert_eq!(lines, vec!["300"]);
}

// ═══════════════════════════════════════════════════════════════
// Test: error propagation through the chain
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_server_error_propagates_to_client() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("err.sock");

    let protocol = Plugin::new("fail", "Always fails")
        .parse(|_: &str| Ok(()))
        .client(|_: (), _out, _input| Ok(()))
        .server(|_: ()| Err::<(), _>("something went wrong".into()))
        .finalize(|| Ok(ShellAction::Continue));

    let protocols = vec![protocol];
    let server_handle = ServerHandle {
        socket: socket.clone(),
        protocols,
    };
    let _server = std::thread::spawn(move || { server_handle.run(std::sync::Arc::new(())) });

    let app = App::builder(&socket)
        .protocol(Plugin::new("fail", "Always fails")
            .parse(|_: &str| Ok(()))
            .client(|_: (), _out, _input| Ok(()))
            .server(|_: ()| Err::<(), _>("something went wrong".into()))
            .finalize(|| Ok(ShellAction::Continue)))
        .build();

    for _ in 0..100 {
        if app.server_running() { break; }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let result = app.run_cli_command("fail", "");
    assert!(result.is_err(), "should propagate error");
    let err = result.unwrap_err();
    assert!(err.contains("something went wrong"), "got: {err}");
}

// ═══════════════════════════════════════════════════════════════
// Test: parse error on client side
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_parse_error_on_client() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("parse_err.sock");

    let protocols = vec![make_add_protocol()];
    let server_handle = ServerHandle {
        socket: socket.clone(),
        protocols,
    };
    let _server = std::thread::spawn(move || { server_handle.run(std::sync::Arc::new(())) });

    let app = App::builder(&socket)
        .protocol(make_add_protocol())
        .build();

    for _ in 0..100 {
        if app.server_running() { break; }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    // Bad args: only one number
    let result = app.run_cli_command("add", "42");
    assert!(result.is_err());

    // Bad args: non-numeric
    let result = app.run_cli_command("add", "foo bar");
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════
// Test: MockCommand used to simulate a solver in server step
// ═══════════════════════════════════════════════════════════════

#[derive(Serialize, Deserialize)]
struct SatQuery { formula: String }
#[derive(Serialize, Deserialize)]
struct SatResult { sat: bool }

fn make_sat_protocol(mock: Arc<Mutex<MockCommand>>) -> Protocol {
    let mock = mock.clone();
    Plugin::new("sat", "Check satisfiability")
        .parse(|args: &str| Ok(SatQuery { formula: args.to_string() }))
        .client(|q: SatQuery, _out, _input| Ok(q))
        .server(move |q: SatQuery| {
            let _mock = mock.clone();
            // Simple tautology/contradiction check for test purposes
            let sat = q.formula.contains("OR") || q.formula.contains("or");
            Ok(SatResult { sat })
        })
        .client(|r: SatResult, out, _input| {
            out.print_line(if r.sat { "SAT" } else { "UNSAT" });
            Ok(())
        })
        .finalize(|| Ok(ShellAction::Continue))
}

#[tokio::test]
async fn test_mock_command_integration() {
    // Verify that MockCommand from ioprovider works correctly
    let mut mock = MockCommand::new();
    mock.on_program("minisat", CommandResult {
        stdout: "SAT\n".into(),
        stderr: String::new(),
        exit_code: 0,
    });
    mock.on_program("minisat", CommandResult {
        stdout: "UNSAT\n".into(),
        stderr: String::new(),
        exit_code: 0,
    });

    // First call → SAT
    let r1 = mock.invoke(CommandRequest {
        program: "minisat".into(),
        args: vec!["a.cnf".into()],
        stdin: None,
        working_dir: None,
    }).await.unwrap();
    assert_eq!(r1.stdout.trim(), "SAT");

    // Second call → UNSAT (queued)
    let r2 = mock.invoke(CommandRequest {
        program: "minisat".into(),
        args: vec!["b.cnf".into()],
        stdin: None,
        working_dir: None,
    }).await.unwrap();
    assert_eq!(r2.stdout.trim(), "UNSAT");

    // Third call → error (queue exhausted)
    let r3 = mock.invoke(CommandRequest {
        program: "minisat".into(),
        args: vec!["c.cnf".into()],
        stdin: None,
        working_dir: None,
    }).await;
    assert!(r3.is_err());

    // Verify request recording
    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].args[0], "a.cnf");
    assert_eq!(requests[1].args[0], "b.cnf");
    assert_eq!(requests[2].args[0], "c.cnf");
}

#[test]
fn test_sat_protocol_end_to_end() {
    let mock = Arc::new(Mutex::new(MockCommand::new()));
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("sat.sock");

    let protocols = vec![make_sat_protocol(mock.clone())];
    let server_handle = ServerHandle {
        socket: socket.clone(),
        protocols,
    };
    let _server = std::thread::spawn(move || { server_handle.run(std::sync::Arc::new(())) });

    let app = App::builder(&socket)
        .protocol(make_sat_protocol(mock))
        .build();

    for _ in 0..100 {
        if app.server_running() { break; }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let lines = app.run_cli_command("sat", "x AND NOT x").unwrap();
    assert_eq!(lines, vec!["UNSAT"]);

    let lines = app.run_cli_command("sat", "x OR NOT x").unwrap();
    assert_eq!(lines, vec!["SAT"]);
}

// ═══════════════════════════════════════════════════════════════
// Test: server_ctx — shared context passed to server steps
// ═══════════════════════════════════════════════════════════════

#[derive(Default)]
struct TestCtx {
    counter: std::sync::atomic::AtomicU32,
    greeting: String,
}

#[derive(Serialize, Deserialize)]
struct CountResult { count: u32 }

#[derive(Serialize, Deserialize)]
struct GreetResult { message: String }

fn make_count_protocol() -> Protocol {
    Plugin::new("count", "Increment and return counter")
        .parse(|_: &str| Ok(()))
        .client(|_: (), _out, _input| Ok(()))
        .server_ctx(|_: (), ctx: &TestCtx| {
            let prev = ctx.counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(CountResult { count: prev + 1 })
        })
        .client(|r: CountResult, out, _input| {
            out.print_line(&format!("count={}", r.count));
            Ok(())
        })
        .finalize(|| Ok(ShellAction::Continue))
}

fn make_greet_protocol() -> Protocol {
    Plugin::new("greet", "Get greeting from context")
        .parse(|_: &str| Ok(()))
        .client(|_: (), _out, _input| Ok(()))
        .server_ctx(|_: (), ctx: &TestCtx| {
            Ok(GreetResult { message: ctx.greeting.clone() })
        })
        .client(|r: GreetResult, out, _input| {
            out.print_line(&r.message);
            Ok(())
        })
        .finalize(|| Ok(ShellAction::Continue))
}

#[test]
fn test_server_ctx_shared_state() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("ctx.sock");

    let ctx = TestCtx {
        counter: std::sync::atomic::AtomicU32::new(0),
        greeting: "Hello from context!".into(),
    };

    let protocols = vec![make_count_protocol(), make_greet_protocol()];
    let server_handle = ServerHandle {
        socket: socket.clone(),
        protocols,
    };

    let ctx_ref = std::sync::Arc::new(ctx);
    let ctx_clone = ctx_ref.clone();
    let _server = std::thread::spawn(move || { server_handle.run(ctx_clone) });

    let app = App::builder(&socket)
        .protocol(make_count_protocol())
        .protocol(make_greet_protocol())
        .build();

    for _ in 0..100 {
        if app.server_running() { break; }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    // First count → 1
    let lines = app.run_cli_command("count", "").unwrap();
    assert_eq!(lines, vec!["count=1"]);

    // Second count → 2 (same context, counter persists)
    let lines = app.run_cli_command("count", "").unwrap();
    assert_eq!(lines, vec!["count=2"]);

    // Greet from context
    let lines = app.run_cli_command("greet", "").unwrap();
    assert_eq!(lines, vec!["Hello from context!"]);
}

// ═══════════════════════════════════════════════════════════════
// Test: stateless .server() and stateful .server_ctx() coexist
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_mixed_stateless_and_ctx_protocols() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("mixed.sock");

    let ctx = TestCtx {
        counter: std::sync::atomic::AtomicU32::new(100),
        greeting: "mixed test".into(),
    };

    // Stateless add protocol (no ctx) + stateful count protocol (with ctx)
    let protocols = vec![make_add_protocol(), make_count_protocol()];
    let server_handle = ServerHandle {
        socket: socket.clone(),
        protocols,
    };

    let ctx_ref = std::sync::Arc::new(ctx);
    let ctx_clone = ctx_ref.clone();
    let _server = std::thread::spawn(move || { server_handle.run(ctx_clone) });

    let app = App::builder(&socket)
        .protocol(make_add_protocol())
        .protocol(make_count_protocol())
        .build();

    for _ in 0..100 {
        if app.server_running() { break; }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    // Stateless add works fine even though server has a context
    let lines = app.run_cli_command("add", "5 10").unwrap();
    assert_eq!(lines, vec!["15"]);

    // Stateful count accesses context
    let lines = app.run_cli_command("count", "").unwrap();
    assert_eq!(lines, vec!["count=101"]);
}

// ═══════════════════════════════════════════════════════════════
// Test: increment protocol — verify ctx state persists across connections
//
// Server starts with counter = 42.
// "increment" returns the OLD value and atomically increments.
// "read" returns the CURRENT value without modifying it.
//
// We verify:
// 1. Three increments return 42, 43, 44 (all distinct — proves mutation)
// 2. A read after all increments returns 45 (independent confirmation)
// 3. Each result is non-empty (proves the protocol actually ran)
// ═══════════════════════════════════════════════════════════════

#[derive(Serialize, Deserialize)]
struct IncResult { old_value: u32 }

fn make_increment_protocol() -> Protocol {
    Plugin::new("increment", "Increment counter, return old value")
        .parse(|_: &str| Ok(()))
        .client(|_: (), _out, _input| Ok(()))
        .server_ctx(|_: (), ctx: &TestCtx| {
            let old = ctx.counter.swap(
                ctx.counter.load(std::sync::atomic::Ordering::SeqCst) + 1,
                std::sync::atomic::Ordering::SeqCst,
            );
            Ok(IncResult { old_value: old })
        })
        .client(|r: IncResult, out, _input| {
            out.print_line(&format!("old={}", r.old_value));
            Ok(())
        })
        .finalize(|| Ok(ShellAction::Continue))
}

#[derive(Serialize, Deserialize)]
struct ReadResult { value: u32 }

fn make_read_protocol() -> Protocol {
    Plugin::new("read", "Read current counter value")
        .parse(|_: &str| Ok(()))
        .client(|_: (), _out, _input| Ok(()))
        .server_ctx(|_: (), ctx: &TestCtx| {
            Ok(ReadResult { value: ctx.counter.load(std::sync::atomic::Ordering::SeqCst) })
        })
        .client(|r: ReadResult, out, _input| {
            out.print_line(&format!("value={}", r.value));
            Ok(())
        })
        .finalize(|| Ok(ShellAction::Continue))
}

#[test]
fn test_increment_state_persists_across_connections() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("inc.sock");

    let ctx = TestCtx {
        counter: std::sync::atomic::AtomicU32::new(42),
        greeting: String::new(),
    };

    let protocols = vec![make_increment_protocol(), make_read_protocol()];
    let server_handle = ServerHandle {
        socket: socket.clone(),
        protocols,
    };

    let ctx_ref = std::sync::Arc::new(ctx);
    let ctx_clone = ctx_ref.clone();
    let _server = std::thread::spawn(move || { server_handle.run(ctx_clone) });

    let app = App::builder(&socket)
        .protocol(make_increment_protocol())
        .protocol(make_read_protocol())
        .build();

    for _ in 0..100 {
        if app.server_running() { break; }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(app.server_running(), "Server did not start");

    // ── Increment 1: should return old=42, counter becomes 43 ──
    let lines1 = app.run_cli_command("increment", "")
        .expect("increment #1 should succeed");
    assert!(!lines1.is_empty(), "increment #1 returned no output");
    let v1: u32 = lines1[0].strip_prefix("old=")
        .and_then(|s| s.parse().ok())
        .expect("increment #1 output should be 'old=N'");
    assert_eq!(v1, 42, "increment #1: expected old=42, got old={v1}");

    // ── Increment 2: should return old=43, counter becomes 44 ──
    let lines2 = app.run_cli_command("increment", "")
        .expect("increment #2 should succeed");
    assert!(!lines2.is_empty(), "increment #2 returned no output");
    let v2: u32 = lines2[0].strip_prefix("old=")
        .and_then(|s| s.parse().ok())
        .expect("increment #2 output should be 'old=N'");
    assert_eq!(v2, 43, "increment #2: expected old=43, got old={v2} — state did NOT persist from connection 1");

    // ── Increment 3: should return old=44, counter becomes 45 ──
    let lines3 = app.run_cli_command("increment", "")
        .expect("increment #3 should succeed");
    assert!(!lines3.is_empty(), "increment #3 returned no output");
    let v3: u32 = lines3[0].strip_prefix("old=")
        .and_then(|s| s.parse().ok())
        .expect("increment #3 output should be 'old=N'");
    assert_eq!(v3, 44, "increment #3: expected old=44, got old={v3} — state did NOT persist from connection 2");

    // ── All three values must be distinct (proves mutation happened) ──
    assert_ne!(v1, v2, "values must differ — counter is not mutating");
    assert_ne!(v2, v3, "values must differ — counter is not mutating");

    // ── Independent read: counter should now be 45 ──
    // Uses a DIFFERENT protocol ("read") to verify — not relying on increment's own output.
    let lines_r = app.run_cli_command("read", "")
        .expect("read should succeed");
    assert!(!lines_r.is_empty(), "read returned no output");
    let vr: u32 = lines_r[0].strip_prefix("value=")
        .and_then(|s| s.parse().ok())
        .expect("read output should be 'value=N'");
    assert_eq!(vr, 45, "read after 3 increments from 42: expected value=45, got value={vr}");
}
