//! # servatui
//!
//! Generic server-client framework with TUI/CLI frontends.
//!
//! Define type-safe protocols using a builder chain where `Client<T>` and
//! `Server<T>` alternate, encoding both the conversation flow and where each
//! step executes. The framework handles transport (Unix socket + JSON),
//! serialization, and provides a two-window TUI or CLI for free.
//!
//! ## Quick Start
//!
//! ```
//! use servatui::prelude::*;
//! use serde::{Serialize, Deserialize};
//!
//! // 1. Define your message types
//! #[derive(Serialize, Deserialize)]
//! struct EchoRequest { text: String }
//!
//! #[derive(Serialize, Deserialize)]
//! struct EchoResponse { text: String }
//!
//! impl Message for EchoRequest {
//!     const NAME: &'static str = "echo";
//!     const HELP: &'static str = "Echo text back";
//!     type Response = EchoResponse;
//! }
//! impl Response for EchoResponse {}
//!
//! // 2. Define your contexts
//! struct ServerCtx;
//! struct ClientCtx;
//!
//! // 3. Build the protocol
//! let protocol = Plugin::new("echo", "Echo text back")
//!     .parse(|args: &str| Ok(EchoRequest { text: args.to_string() }))
//!     .client(|req: EchoRequest, _cctx: &mut ClientCtx, _out, _input| Ok(req))
//!     .server(|req: EchoRequest, _ctx: &mut ServerCtx| {
//!         Ok(EchoResponse { text: req.text })
//!     })
//!     .client(|resp: EchoResponse, _cctx: &mut ClientCtx, out, _input| {
//!         out.print_line(&resp.text);
//!         Ok(())
//!     })
//!     .finalize(|_cctx: &mut ClientCtx| Ok(ShellAction::Continue));
//! ```

pub mod connection;
pub mod console;
pub mod protocol;
pub mod server;
pub mod step;

pub use connection::{RawConnection, TypedConnection, SocketConnection};
pub use console::{Console, StdoutConsole, InputSource, StdinInput, BufferConsole};
pub use protocol::{Plugin, Protocol};
pub use server::{App, AppBuilder};
pub use step::{ProtocolStep, ShellAction};

/// Marker trait for message types.
pub trait Message: serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static {
    const NAME: &'static str;
    const HELP: &'static str;
}

/// Marker trait for response types.
pub trait Response: serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static {}

/// Prelude — import this for the most common types.
pub mod prelude {
    pub use crate::*;
}
