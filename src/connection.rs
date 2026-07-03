//! Typed wire transport over Unix domain sockets.

use serde::{de::DeserializeOwned, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

/// Raw byte transport — object-safe (no generics).
/// Use this as `&mut dyn RawConnection` in trait methods.
pub trait RawConnection {
    fn send_bytes(&mut self, data: &[u8]) -> Result<(), String>;
    fn recv_bytes(&mut self) -> Result<Vec<u8>, String>;
}

/// Typed convenience methods built on RawConnection.
/// Call `conn.send_typed::<T>(x)` / `conn.recv_typed::<T>()`.
pub trait TypedConnection: RawConnection {
    fn send_typed<T: Serialize>(&mut self, data: &T) -> Result<(), String> {
        let json = serde_json::to_vec(data).map_err(|e| e.to_string())?;
        self.send_bytes(&json)
    }
    fn recv_typed<T: DeserializeOwned>(&mut self) -> Result<T, String> {
        let bytes = self.recv_bytes()?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }
}

/// Blanket impl: any RawConnection is also TypedConnection.
impl<T: RawConnection + ?Sized> TypedConnection for T {}

/// Unix domain socket connection with JSON serialization.
pub struct SocketConnection {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl SocketConnection {
    pub fn connect(path: &Path) -> Result<Self, String> {
        let stream = UnixStream::connect(path).map_err(|e| e.to_string())?;
        let reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        Ok(Self { stream, reader })
    }

    /// Check if a server is listening at the given path.
    pub fn server_exists(path: &Path) -> bool {
        UnixStream::connect(path).is_ok()
    }
}

impl RawConnection for SocketConnection {
    fn send_bytes(&mut self, data: &[u8]) -> Result<(), String> {
        self.stream.write_all(data).map_err(|e| e.to_string())?;
        self.stream.write_all(b"\n").map_err(|e| e.to_string())?;
        self.stream.flush().map_err(|e| e.to_string())?;
        Ok(())
    }

    fn recv_bytes(&mut self) -> Result<Vec<u8>, String> {
        let mut line = String::new();
        self.reader.read_line(&mut line).map_err(|e| e.to_string())?;
        Ok(line.into_bytes())
    }
}
