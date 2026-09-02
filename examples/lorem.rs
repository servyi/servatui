//! Lorem test app: single `lorem` command that fills one log line with
//! lorem ipsum (long enough to wrap, for testing selection/wrapping).
//!
//! Run as server: `cargo run --example lorem -- serve /tmp/lorem.sock`
//! Run as client: `cargo run --example lorem -- /tmp/lorem.sock`

use serde::{Deserialize, Serialize};
use servyi_servatui::*;
use std::env;

const LOREM: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, \
sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.";

#[derive(Serialize, Deserialize)]
struct LoremResult { text: String }

fn lorem_protocol() -> Protocol {
    Plugin::new("lorem", "Fill one line with lorem ipsum")
        .parse(|_: &str| Ok(()))
        .client(|_: (), _out, _input| Ok(()))
        .server(|_: ()| Ok(LoremResult { text: LOREM.to_string() }))
        .client(|result: LoremResult, out, _input| {
            out.print_line(&result.text);
            Ok(())
        })
        .finalize(|| Ok(ShellAction::Continue))
        // Demo of plugin tab completion: completes the last word; the
        // suggestion appears as a flashing, unconfirmed tail (Tab cycles,
        // ESC deletes, any other key confirms).
        .complete(|s: &str| {
            let (head, last) = s.rsplit_once(' ').unwrap_or(("", s));
            ["ipsum", "dolor", "sit"]
                .iter()
                .filter(|w| w.starts_with(last))
                .map(|w| {
                    if head.is_empty() {
                        (*w).to_string()
                    } else {
                        format!("{head} {w}")
                    }
                })
                .collect()
        })
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let socket = if args.len() >= 3 && args[1] == "serve" {
        args[2].clone()
    } else if args.len() >= 2 {
        args[1].clone()
    } else {
        eprintln!("Usage:");
        eprintln!("  Server: lorem serve <socket>");
        eprintln!("  Client: lorem <socket>");
        std::process::exit(1);
    };

    let app = App::builder(&socket)
        .version("0.1.0")
        .protocol(lorem_protocol())
        .build();

    if args.len() >= 2 && args[1] == "serve" {
        println!("Starting lorem server on {socket}");
        if let Err(e) = app.run_server(std::sync::Arc::new(())) {
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
            eprintln!("TUI feature not enabled. Use: cargo run --example lorem --features tui");
        }
    }
}
