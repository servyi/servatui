//! Echo example: a complete servatui server+client.
//!
//! Run as server: `cargo run --example echo -- serve /tmp/echo.sock`
//! Run as client: `cargo run --example echo -- /tmp/echo.sock`

use serde::{Deserialize, Serialize};
use servyi_servatui::*;
use std::env;

#[derive(Serialize, Deserialize)]
struct EchoArgs { text: String }

#[derive(Serialize, Deserialize)]
struct GreetResult { message: String }

fn echo_protocol() -> Protocol {
    Plugin::new("echo", "Echo text back")
        .parse(|args: &str| Ok(EchoArgs { text: args.to_string() }))
        .client(|args: EchoArgs, _out, _input| Ok(args))
        .server(|args: EchoArgs| Ok(EchoArgs { text: args.text }))
        .client(|result: EchoArgs, out, _input| {
            out.print_line(&result.text);
            Ok(())
        })
        .finalize(|| Ok(ShellAction::Continue))
}

fn greet_protocol() -> Protocol {
    Plugin::new("greet", "Get a greeting")
        .parse(|_: &str| Ok(()))
        .client(|_: (), _out, _input| Ok(()))
        .server(|_: ()| Ok(GreetResult { message: "Hello from servatui!".into() }))
        .client(|result: GreetResult, out, _input| {
            out.print_line(&result.message);
            Ok(())
        })
        .finalize(|| Ok(ShellAction::Continue))
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let socket = if args.len() >= 3 && args[1] == "serve" {
        args[2].clone()
    } else if args.len() >= 2 {
        args[1].clone()
    } else {
        eprintln!("Usage:");
        eprintln!("  Server: echo serve <socket>");
        eprintln!("  Client: echo <socket>");
        std::process::exit(1);
    };

    let app = App::builder(&socket)
        .version("0.1.0")
        .protocol(echo_protocol())
        .protocol(greet_protocol())
        .build();

    if args.len() >= 2 && args[1] == "serve" {
        println!("Starting echo server on {socket}");
        if let Err(e) = app.run_server(&()) {
            eprintln!("Server error: {e}");
            std::process::exit(1);
        }
    } else {
        #[cfg(feature = "tui")]
        {
            if let Err(e) = app.run_tui() {
                eprintln!("Client error: {e}");
                std::process::exit(1);
            }
        }
        #[cfg(not(feature = "tui"))]
        {
            eprintln!("TUI feature not enabled. Use: cargo run --example echo --features tui");
        }
    }
}
