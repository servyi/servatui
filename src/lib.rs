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
#[cfg(feature = "tui")]
pub mod mouse;
pub mod protocol;
pub mod server;
#[cfg(feature = "tui")]
mod terminal_restore;
#[cfg(feature = "tui")]
pub mod tui;

pub use connection::{RawConnection, TypedConnection, SocketConnection};
pub use console::{Console, InputSource, StdoutConsole, StdinInput, BufferConsole, NoInput};
pub use protocol::{Plugin, Protocol, ShellAction, Client, Server, ClientHead, ClientBuilder, ServerBuilder};
pub use server::{App, AppBuilder, ServerHandle};
#[cfg(feature = "tui")]
// run_tui is deprecated (removal after October 26) and no longer re-exported
// at the crate root; use run_tui_with_events or the servatui-display crate.
pub use tui::{run_tui_managed, run_tui_with_events, run_tui_with_overlay, BuiltinTui, TuiState, WidgetEntry, ScrollbarWidget, WIDGET_LOG, WIDGET_INPUT, WIDGET_SCROLLBAR};
#[cfg(feature = "tui")]
pub use mouse::MouseTracker;
