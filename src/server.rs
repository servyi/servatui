//! Server and client runtime.

use std::any::Any;
use std::path::{Path, PathBuf};

use crate::connection::{SocketConnection, TypedConnection, RawConnection};
use crate::console::{BufferConsole, NoInput};
use crate::protocol::Protocol;

/// Server handle — holds protocols and listens on a socket.
pub struct ServerHandle {
    pub socket: PathBuf,
    pub protocols: Vec<Protocol>,
}

impl ServerHandle {
    /// Run the server loop. Blocks.
    /// `ctx` is shared state passed to all `.server_ctx()` steps.
    pub fn run<Ctx: 'static>(self, ctx: &Ctx) -> Result<(), String> {
        let ctx_any: &dyn Any = ctx;
        let _ = std::fs::remove_file(&self.socket);
        let listener = std::os::unix::net::UnixListener::bind(&self.socket)
            .map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.socket, std::fs::Permissions::from_mode(0o666)).ok();
        }

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let reader = stream.try_clone().map_err(|e| e.to_string())?;
                    let mut conn = SocketConnection {
                        stream,
                        reader: std::io::BufReader::new(reader),
                    };
                    if let Err(e) = self.handle_connection(&mut conn, ctx_any) {
                        eprintln!("Connection error: {e}");
                    }
                }
                Err(e) => eprintln!("Accept error: {e}"),
            }
        }
        Ok(())
    }

    fn handle_connection(&self, conn: &mut SocketConnection, ctx: &dyn Any) -> Result<(), String> {
        let raw = conn.recv_bytes()?;
        let trimmed = std::str::from_utf8(&raw).unwrap_or("").trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let cmd_name: String = serde_json::from_str(trimmed)
            .map_err(|e| e.to_string())?;
        let proto = self.protocols.iter()
            .find(|p| p.name == cmd_name)
            .ok_or_else(|| format!("Unknown command: {cmd_name}"))?;
        proto.run_server(conn, ctx)
    }
}

/// Builder for assembling an App with protocols.
pub struct AppBuilder {
    socket: PathBuf,
    version: String,
    log_path: String,
    protocols: Vec<Protocol>,
}

impl AppBuilder {
    pub fn new(socket: impl AsRef<Path>) -> Self {
        Self {
            socket: socket.as_ref().to_path_buf(),
            version: "0.1.0".to_string(),
            log_path: String::new(),
            protocols: Vec::new(),
        }
    }

    pub fn version(mut self, v: impl Into<String>) -> Self { self.version = v.into(); self }
    pub fn log_path(mut self, p: impl Into<String>) -> Self { self.log_path = p.into(); self }
    pub fn protocol(mut self, p: Protocol) -> Self { self.protocols.push(p); self }
    pub fn protocol_all(mut self, protocols: impl IntoIterator<Item = Protocol>) -> Self {
        self.protocols.extend(protocols);
        self
    }

    pub fn build(self) -> App {
        App {
            socket: self.socket,
            version: self.version,
            log_path: self.log_path,
            protocols: self.protocols,
        }
    }
}

/// The assembled application.
pub struct App {
    pub socket: PathBuf,
    pub version: String,
    pub log_path: String,
    pub protocols: Vec<Protocol>,
}

impl App {
    pub fn builder(socket: impl AsRef<Path>) -> AppBuilder { AppBuilder::new(socket) }

    /// Run as a server. Blocks.
    /// `ctx` is shared state passed to all `.server_ctx()` steps.
    pub fn run_server<Ctx: 'static>(self, ctx: &Ctx) -> Result<(), String> {
        ServerHandle { socket: self.socket, protocols: self.protocols }.run(ctx)
    }

    /// Run a single command as a CLI client.
    /// Returns rendered output lines.
    pub fn run_cli_command(&self, command: &str, args: &str) -> Result<Vec<String>, String> {
        Ok(self.run_cli_command_raw(command, args)?.0)
    }

    /// Run a single command as a CLI client.
    /// Returns (rendered lines, raw server response bytes).
    pub fn run_cli_command_raw(&self, command: &str, args: &str) -> Result<(Vec<String>, Vec<u8>), String> {
        let mut conn = SocketConnection::connect(&self.socket)?;
        conn.send_typed(&command.to_string())?;
        let proto = self.protocols.iter()
            .find(|p| p.name == command)
            .ok_or_else(|| format!("Unknown command: {command}"))?;
        let mut console = BufferConsole::new();
        let mut input = NoInput;
        let raw = proto.run_client(args, &mut conn, &mut console, &mut input)?;
        Ok((console.lines, raw))
    }

    /// Run the TUI client. Blocks until user exits.
    #[cfg(feature = "tui")]
    pub fn run_tui(&self) -> Result<(), String> {
        crate::tui::run_tui(&self.socket, &self.protocols)
    }

    /// Check if server is running.
    pub fn server_running(&self) -> bool {
        SocketConnection::server_exists(&self.socket)
    }

    /// List registered commands.
    pub fn commands(&self) -> Vec<(&'static str, &'static str)> {
        self.protocols.iter().map(|p| (p.name, p.help)).collect()
    }
}
