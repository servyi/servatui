//! Terminal size diagnostic.
//!
//! Run: cargo run --example size-check --features tui

use std::io::Write;

fn main() {
    // Before raw mode
    let cols = std::env::var("COLUMNS").unwrap_or_else(|_| "<not set>".into());
    let lines = std::env::var("LINES").unwrap_or_else(|_| "<not set>".into());
    let stty = std::process::Command::new("stty").arg("size").output();
    let stty_str = stty.ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| "<failed>".into());

    println!("=== Before raw mode ===");
    println!("crossterm::terminal::size(): {:?}", crossterm::terminal::size());
    println!("stty size: {}", stty_str.trim());
    println!("$COLUMNS={}", cols);
    println!("$LINES={}", lines);
    println!("stdout is tty: {}", isatty(1));
    println!("stderr is tty: {}", isatty(2));
    println!("stdin is tty: {}", isatty(0));
    println!();

    // Enter raw mode + alternate screen (same as TUI does)
    crossterm::terminal::enable_raw_mode().unwrap();
    crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen).unwrap();
    std::io::stdout().flush().ok();

    let size_after = crossterm::terminal::size();
    let cols_after = std::env::var("COLUMNS").unwrap_or_else(|_| "<not set>".into());
    let lines_after = std::env::var("LINES").unwrap_or_else(|_| "<not set>".into());

    // Write the size to stderr so it's visible even in alternate screen
    eprintln!("=== After raw mode + alternate screen ===");
    eprintln!("crossterm::terminal::size(): {:?}", size_after);
    eprintln!("$COLUMNS={}", cols_after);
    eprintln!("$LINES={}", lines_after);

    // Exit
    crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen).ok();
    crossterm::terminal::disable_raw_mode().ok();

    println!();
    println!("=== After exit ===");
    println!("crossterm::terminal::size(): {:?}", crossterm::terminal::size());
}

fn isatty(fd: i32) -> bool {
    unsafe { libc::isatty(fd) != 0 }
}
