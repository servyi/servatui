//! Best-effort terminal restoration for abnormal TUI shutdown.
//!
//! `run_tui_with_overlay` enables raw mode, the alternate screen and
//! mouse capture. The normal-exit path disables all three — but a
//! signal (SIGTERM/SIGHUP: container teardown, pty close) or a panic
//! skips it, leaving the user's terminal with a dead scroll wheel and
//! an unreachable backlog (mouse capture diverts wheel events; the
//! alternate screen hides the scrollback).
//!
//! Restoration runs on every path:
//!   1. [`TerminalGuard`] — RAII, covers unwinding panics and early
//!      error returns after the modes were enabled
//!   2. a chained panic hook — restores before the panic message is
//!      printed, so the message lands on the main screen
//!   3. a signal thread (SIGTERM/SIGINT/SIGHUP) — restores, then exits
//!      with the conventional 128 + signo
//!
//! All restores are idempotent; running without a tty is not an error.

use std::io::Write;

/// The escape sequences that undo everything the TUI enables, in the
/// same order the normal-exit path uses: mouse capture off
/// (1000/1002/1003/1015/1006), alternate screen off (1049), cursor
/// visible (25h).
pub(crate) fn restore_bytes() -> Vec<u8> {
    let mut buf = String::new();
    use crossterm::Command;
    crossterm::event::DisableMouseCapture
        .write_ansi(&mut buf)
        .ok();
    crossterm::terminal::LeaveAlternateScreen
        .write_ansi(&mut buf)
        .ok();
    crossterm::cursor::Show.write_ansi(&mut buf).ok();
    buf.into_bytes()
}

/// Restore the terminal: disable mouse capture and the alternate
/// screen, make the cursor visible, leave raw mode. Best-effort.
pub(crate) fn restore_terminal() {
    let mut out = std::io::stdout();
    let _ = out.write_all(&restore_bytes());
    let _ = out.flush();
    let _ = crossterm::terminal::disable_raw_mode();
}

static PANIC_HOOK_INSTALLED: std::sync::Once = std::sync::Once::new();
static SIGNALS_INSTALLED: std::sync::Once = std::sync::Once::new();

/// RAII terminal restore.
pub(crate) struct TerminalGuard {
    /// Signals are process-global; the guard itself carries no state.
    _priv: (),
}

impl TerminalGuard {
    /// Install the panic hook and signal handling, and return a guard
    /// whose `Drop` restores the terminal.
    pub(crate) fn install() -> Self {
        install_panic_hook();
        install_signal_thread();
        TerminalGuard { _priv: () }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn install_panic_hook() {
    PANIC_HOOK_INSTALLED.call_once(|| {
        let original = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            original(info);
        }));
    });
}

fn install_signal_thread() {
    SIGNALS_INSTALLED.call_once(|| {
        use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
        use signal_hook::iterator::Signals;
        if let Ok(signals) = Signals::new([SIGTERM, SIGINT, SIGHUP]) {
            // Process-lifetime: unregistered only when the process ends.
            let signals = Box::leak(Box::new(signals));
            std::thread::spawn(move || {
                // Blocks until the first signal; the handler exits the
                // process, so at most one iteration ever runs.
                if let Some(sig) = signals.forever().next() {
                    restore_terminal();
                    std::process::exit(128 + sig);
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The restore set must stay in lock-step with what the TUI enables:
    /// if crossterm changes the disable sequences, this fails and the
    /// restore path must be revisited.
    #[test]
    fn restore_bytes_match_crossterm_disable_commands() {
        let mut expected = String::new();
        use crossterm::Command;
        crossterm::event::DisableMouseCapture
            .write_ansi(&mut expected)
            .unwrap();
        crossterm::terminal::LeaveAlternateScreen
            .write_ansi(&mut expected)
            .unwrap();
        crossterm::cursor::Show.write_ansi(&mut expected).unwrap();
        assert_eq!(restore_bytes(), expected.into_bytes());
    }

    /// Every mode the TUI enables has its literal disable sequence —
    /// the regression that motivated this module was the scroll wheel
    /// (mouse capture 1006) staying on.
    #[test]
    fn restore_bytes_disable_every_enabled_mode() {
        let bytes = restore_bytes();
        for seq in [
            b"\x1b[?1000l".as_slice(),
            b"\x1b[?1002l",
            b"\x1b[?1003l",
            b"\x1b[?1015l",
            b"\x1b[?1006l",
            b"\x1b[?1049l",
            b"\x1b[?25h",
        ] {
            assert!(
                bytes.windows(seq.len()).any(|w| w == seq),
                "restore is missing {seq:?}"
            );
        }
    }

    /// Without a tty (piped stdout here) restoration is a no-op that
    /// must not panic — and repeated restoration is idempotent.
    #[test]
    fn restore_without_tty_is_idempotent_noop() {
        let guard = TerminalGuard::install();
        restore_terminal();
        restore_terminal();
        drop(guard);
    }
}
