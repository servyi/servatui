//! Typed wire transport over Unix domain sockets.

use serde::{de::DeserializeOwned, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

/// Raw byte transport — object-safe (no generics).
pub trait RawConnection {
    fn send_bytes(&mut self, data: &[u8]) -> Result<(), String>;
    fn recv_bytes(&mut self) -> Result<Vec<u8>, String>;
}

/// Typed convenience methods built on RawConnection.
pub trait TypedConnection: RawConnection {
    fn send_typed<T: Serialize>(&mut self, data: &T) -> Result<(), String> {
        let json = serde_json::to_vec(data).map_err(|e| e.to_string())?;
        self.send_bytes(&json)
    }
    fn recv_typed<T: DeserializeOwned>(&mut self) -> Result<T, String> {
        let bytes = self.recv_bytes()?;
        let s = std::str::from_utf8(&bytes).unwrap_or("").trim();
        serde_json::from_str(s).map_err(|e| e.to_string())
    }
}

/// Blanket impl.
impl<T: RawConnection + ?Sized> TypedConnection for T {}

/// Name of the running binary, for usage hints in error messages.
fn current_bin_name() -> String {
    std::env::args().next()
        .map(|a| {
            std::path::Path::new(&a)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or(a.clone())
        })
        .unwrap_or_else(|| "app".into())
}

/// Map a connect(2) failure to an actionable message.
fn explain_connect_error(path: &Path, err: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    match err.kind() {
        NotFound => format!(
            "no server is running at `{}` — start one with `{} serve {}`",
            path.display(),
            current_bin_name(),
            path.display()
        ),
        ConnectionRefused => format!(
            "server at `{}` is not accepting connections — a crashed server \
             leaves a stale socket file behind; remove it (`rm {}`) and restart",
            path.display(),
            path.display()
        ),
        PermissionDenied => format!("permission denied connecting to `{}`", path.display()),
        _ => err.to_string(),
    }
}

/// Map an I/O failure mid-conversation to an actionable message.
fn explain_io_error(err: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    match err.kind() {
        BrokenPipe | ConnectionReset | ConnectionAborted => "server closed the connection".into(),
        _ => err.to_string(),
    }
}

/// Unix domain socket connection with JSON serialization.
pub struct SocketConnection {
    pub stream: UnixStream,
    pub reader: BufReader<UnixStream>,
}

impl SocketConnection {
    pub fn connect(path: &Path) -> Result<Self, String> {
        if path.as_os_str().is_empty() {
            // Linux maps this to EINVAL ("Invalid argument") because an empty
            // sun_path is a zero-length abstract-socket name — opaque to users.
            return Err(format!(
                "socket path is empty (start a server with `{} serve <socket>`)",
                current_bin_name()
            ));
        }
        let stream = UnixStream::connect(path).map_err(|e| explain_connect_error(path, &e))?;
        let reader = BufReader::new(
            stream
                .try_clone()
                .map_err(|e| explain_io_error(&e))?,
        );
        Ok(Self { stream, reader })
    }

    pub fn server_exists(path: &Path) -> bool {
        UnixStream::connect(path).is_ok()
    }
}

impl RawConnection for SocketConnection {
    fn send_bytes(&mut self, data: &[u8]) -> Result<(), String> {
        self.stream.write_all(data).map_err(|e| explain_io_error(&e))?;
        self.stream.write_all(b"\n").map_err(|e| explain_io_error(&e))?;
        self.stream.flush().map_err(|e| explain_io_error(&e))?;
        Ok(())
    }

    fn recv_bytes(&mut self) -> Result<Vec<u8>, String> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).map_err(|e| explain_io_error(&e))?;
        if n == 0 {
            return Err("connection closed".into());
        }
        Ok(line.into_bytes())
    }
}

/// In-memory endpoint for unit tests.
pub struct TestEndpoint {
    pub incoming: std::sync::Mutex<std::collections::VecDeque<Vec<u8>>>,
    pub outgoing: std::sync::Mutex<std::collections::VecDeque<Vec<u8>>>,
}

impl RawConnection for TestEndpoint {
    fn send_bytes(&mut self, data: &[u8]) -> Result<(), String> {
        self.outgoing.lock().unwrap().push_back(data.to_vec());
        Ok(())
    }
    fn recv_bytes(&mut self) -> Result<Vec<u8>, String> {
        self.incoming.lock().unwrap().pop_front().ok_or("no data".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_socket_path_gives_actionable_error() {
        let err = match SocketConnection::connect(Path::new("")) {
            Err(e) => e,
            Ok(_) => panic!("empty path must never connect"),
        };
        assert!(
            err.contains("socket path is empty"),
            "expected actionable message, got: {err}"
        );
        assert!(
            !err.contains("Invalid argument"),
            "must not leak the raw EINVAL: {err}"
        );
        assert!(err.contains("serve"), "should point at the fix: {err}");
    }

    #[test]
    fn missing_socket_gives_actionable_error() {
        // Path in a nonexistent directory: real ENOENT from the OS.
        let err = match SocketConnection::connect(Path::new("/nonexistent-dir-xyz/s.sock")) {
            Err(e) => e,
            Ok(_) => panic!("must not connect"),
        };
        assert!(
            err.contains("no server") && err.contains("serve"),
            "expected actionable message, got: {err}"
        );
        assert!(
            !err.contains("No such file or directory"),
            "must not leak the raw ENOENT: {err}"
        );
    }

    #[test]
    fn stale_socket_file_gives_actionable_error() {
        // A regular FILE where the socket should be: Linux refuses with
        // ECONNREFUSED, same errno a crashed server's leftover socket gives.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stale.sock");
        std::fs::write(&path, b"not a socket").unwrap();
        let err = match SocketConnection::connect(&path) {
            Err(e) => e,
            Ok(_) => panic!("must not connect"),
        };
        assert!(
            err.contains("stale") || err.contains("restart"),
            "expected actionable message, got: {err}"
        );
        assert!(
            !err.contains("Connection refused"),
            "must not leak the raw ECONNREFUSED: {err}"
        );
    }

    #[test]
    fn permission_denied_maps_to_hint() {
        let e = std::io::Error::from_raw_os_error(13); // EACCES
        let msg = explain_connect_error(Path::new("/x/sock"), &e);
        assert!(
            msg.contains("permission denied") && msg.contains("/x/sock"),
            "got: {msg}"
        );
    }

    #[test]
    fn unknown_connect_errno_falls_back_to_display() {
        let e = std::io::Error::from_raw_os_error(75); // EOVERFLOW, unmapped
        let msg = explain_connect_error(Path::new("/x/sock"), &e);
        assert_eq!(msg, e.to_string());
    }

    #[test]
    fn broken_pipe_on_send_means_server_gone() {
        let e = std::io::Error::from_raw_os_error(32); // EPIPE
        let msg = explain_io_error(&e);
        assert_eq!(msg, "server closed the connection");
    }

    #[test]
    fn unknown_io_errno_falls_back_to_display() {
        let e = std::io::Error::from_raw_os_error(75);
        assert_eq!(explain_io_error(&e), e.to_string());
    }
}
