//! Panic-inside-TUI smoke test.
//!
//! Used by tests/terminal_restore.rs: the TUI must restore the terminal
//! (mouse capture off, main screen) even when the process dies from a
//! panic during rendering.
//!
//! Run: cargo run --features tui --example panic-smoke -- /tmp/x.sock

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let socket = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "/tmp/panic-smoke.sock".into());

    #[cfg(feature = "tui")]
    {
        let protocol = servyi_servatui::Plugin::new("noop", "do nothing")
            .parse(|_: &str| Ok(()))
            .client(|_: (), _, _| Ok(()))
            .server(|_: ()| Ok(()))
            .client(|_: (), _, _| Ok(()))
            .finalize(|| Ok(servyi_servatui::ShellAction::Continue));
        servyi_servatui::run_tui_with_overlay(
            &std::path::PathBuf::from(socket),
            &[protocol],
            |_| panic!("panic-smoke: intentional panic during TUI render"),
        )
        .unwrap();
    }
    #[cfg(not(feature = "tui"))]
    {
        let _ = socket;
        eprintln!("TUI feature not enabled.");
        std::process::exit(1);
    }
}
