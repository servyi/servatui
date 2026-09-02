//! Terminal-restore regression tests.
//!
//! Bug: if the TUI process dies by signal (e.g. its container is killed
//! externally) or panics, the user's terminal is left in mouse-capture +
//! alternate-screen mode — the scroll wheel is dead and the backlog is
//! unreachable (mouse capture diverts wheel events; the alternate screen
//! hides the scrollback). Restore must happen on those paths too, not
//! just on normal exit.
//!
//! These tests run a real TUI under a real pty (util-linux `script`),
//! kill it with SIGTERM (resp. let it panic), and assert the disable
//! sequences (`\x1b[?1006l`, `\x1b[?1049l`) reach the pty.
//!
//! Requires: Linux (util-linux script) and the default `tui` feature.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn example(name: &str) -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/debug/examples")
        .join(name);
    assert!(
        p.exists(),
        "example binary not built: {} (run `cargo test`)",
        p.display()
    );
    p
}

fn wait_until(deadline_ms: u128, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed().as_millis() < deadline_ms {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    cond()
}

fn typescript_contains(path: &Path, needle: &[u8]) -> bool {
    let Ok(mut f) = fs::File::open(path) else {
        return false;
    };
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return false;
    }
    buf.windows(needle.len()).any(|w| w == needle)
}

/// Find the pid whose argv is exactly `[exe, socket]`.
///
/// `script -c` runs `sh -c "<cmd>"`, and dash execs single commands, so
/// script's child IS the example binary with exactly that argv. Matching
/// the full cmdline avoids matching the `script` process itself (whose
/// argv also contains the command string).
fn find_client(exe: &Path, socket: &Path) -> Option<u32> {
    let wanted = format!("{}\0{}\0", exe.display(), socket.display()).into_bytes();
    let entries = fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if fs::read(format!("/proc/{pid}/cmdline")).ok() == Some(wanted.clone()) {
            return Some(pid);
        }
    }
    None
}

fn spawn_under_pty(cmd: &str, typescript: &Path) -> std::process::Child {
    Command::new("script")
        .arg("-qec")
        .arg(cmd)
        .arg(typescript)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("util-linux `script` is required for the pty tests")
}

fn poll_exit(child: &mut std::process::Child, deadline_ms: u128) -> Option<std::process::ExitStatus> {
    let mut status = None;
    wait_until(deadline_ms, || match child.try_wait() {
        Ok(Some(s)) => {
            status = Some(s);
            true
        }
        Ok(None) => false,
        Err(_) => true,
    });
    status
}

fn cleanup(mut child: std::process::Child) {
    if let Ok(None) = child.try_wait() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[test]
fn sigterm_restores_mouse_capture_and_alt_screen() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("s.sock");
    let ts = dir.path().join("ts");
    let exe = example("echo");

    let mut script = spawn_under_pty(&format!("{} {}", exe.display(), sock.display()), &ts);

    // Sync point: the TUI has enabled mouse capture (and is rendering).
    assert!(
        wait_until(15_000, || typescript_contains(&ts, b"\x1b[?1006h")),
        "TUI never enabled mouse capture — did it start?"
    );
    let pid = loop {
        assert!(
            wait_until(5_000, || find_client(&exe, &sock).is_some()),
            "client process not found under the pty"
        );
        if let Some(pid) = find_client(&exe, &sock) {
            break pid;
        }
    };

    Command::new("kill")
        .arg(pid.to_string())
        .status()
        .expect("kill failed");

    let exited = poll_exit(&mut script, 10_000);
    let capture_off = typescript_contains(&ts, b"\x1b[?1006l");
    let alt_off = typescript_contains(&ts, b"\x1b[?1049l");
    cleanup(script);

    assert!(exited.is_some(), "script session did not exit after SIGTERM");
    assert!(
        capture_off,
        "mouse capture not disabled on SIGTERM — terminal left with dead scroll wheel"
    );
    assert!(
        alt_off,
        "alternate screen not left on SIGTERM — backlog unreachable"
    );
}

#[test]
fn panic_restores_mouse_capture_and_alt_screen() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("s.sock");
    let ts = dir.path().join("ts");
    let exe = example("panic-smoke");

    let mut script = spawn_under_pty(&format!("{} {}", exe.display(), sock.display()), &ts);

    let status = poll_exit(&mut script, 15_000);
    let capture_off = typescript_contains(&ts, b"\x1b[?1006l");
    let alt_off = typescript_contains(&ts, b"\x1b[?1049l");
    cleanup(script);

    let status = status.expect("panic-smoke session did not exit");
    assert_eq!(
        status.code(),
        Some(101),
        "panic-smoke should die from the panic (exit 101), got {status}"
    );
    assert!(
        capture_off,
        "mouse capture not disabled on panic — terminal left with dead scroll wheel"
    );
    assert!(
        alt_off,
        "alternate screen not left on panic — backlog unreachable"
    );
}
