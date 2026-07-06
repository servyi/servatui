//! # servatui
//!
//! Generic server-client framework with TUI/CLI frontends.
//!
//! Define type-safe protocols using a builder chain where `Client<T>` and
//! `Server<T>` alternate, encoding both the conversation flow and where each
//! step executes. The framework handles transport (Unix socket + JSON),
//! serialization, and provides a TUI or CLI frontend for free.

pub mod connection;
pub mod console;
pub mod protocol;
pub mod server;
#[cfg(feature = "tui")]
pub mod tui;

pub use connection::{RawConnection, TypedConnection, SocketConnection};
pub use console::{Console, InputSource, StdoutConsole, StdinInput, BufferConsole, NoInput};
pub use protocol::{Plugin, Protocol, ShellAction, Client, Server, ClientHead, ClientBuilder, ServerBuilder};
pub use server::{App, AppBuilder, ServerHandle};
#[cfg(feature = "tui")]
pub use tui::{run_tui, TuiState};
