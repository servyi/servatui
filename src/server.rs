//! App builder — assembles protocols and runs as server or client.

use std::path::PathBuf;

use crate::protocol::Protocol;

/// Builder for constructing an App with protocols.
pub struct AppBuilder {
    socket: PathBuf,
    version: String,
    log_path: String,
    protocols: Vec<Protocol>,
}

impl AppBuilder {
    pub fn new(socket: impl AsRef<std::path::Path>) -> Self {
        Self {
            socket: socket.as_ref().to_path_buf(),
            version: "0.1.0".to_string(),
            log_path: String::new(),
            protocols: Vec::new(),
        }
    }

    pub fn version(mut self, v: impl Into<String>) -> Self {
        self.version = v.into();
        self
    }

    pub fn log_path(mut self, p: impl Into<String>) -> Self {
        self.log_path = p.into();
        self
    }

    pub fn protocol(mut self, p: Protocol) -> Self {
        self.protocols.push(p);
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
    pub fn builder(socket: impl AsRef<std::path::Path>) -> AppBuilder {
        AppBuilder::new(socket)
    }

    /// Run as a server. Blocks.
    pub fn run_server<Ctx: Send>(self, _ctx: Ctx) -> Result<(), String> {
        // TODO: socket listener, dispatch, version/commands discovery
        Ok(())
    }

    /// Run the TUI client. Blocks.
    pub fn run_tui(self) -> Result<(), String> {
        // TODO: two-window layout, plugin dispatch, event loop
        Ok(())
    }

    /// Run in CLI mode (single command, stdout output).
    pub fn run_cli(self) -> Result<(), String> {
        // TODO: parse args, dispatch, print
        Ok(())
    }
}
