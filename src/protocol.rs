//! Builder chain: Plugin → Client ⇄ Server → Protocol
//!
//! Wire protocol (alternating C→S, S→C):
//!   C→S: command name (string)
//!   C→S: output of client step 1
//!   S→C: output of server step 1
//!   C→S: output of client step 2
//!   S→C: output of server step 2
//!   ...
//!   C→S: sentinel ()
//!
//! Server steps come in two flavors:
//! - `.server(|req| ...)` — stateless, no context access
//! - `.server_ctx(|req, ctx: &DaemonState| ...)` — stateful, shared server context

use std::any::Any;
use std::marker::PhantomData;
use std::sync::Arc;

use serde::{de::DeserializeOwned, Serialize};

use crate::connection::{RawConnection, TypedConnection};
use crate::console::{Console, InputSource};

/// Type-erased parse closure shared across the builder chain.
type ParseFn = Arc<dyn Fn(&str) -> Result<Vec<u8>, String> + Send + Sync>;
/// Type-erased offline-fallback closure (see [`Protocol::offline`]).
type OfflineFn = Arc<dyn Fn(&str, &mut dyn Console) -> Result<(), String> + Send + Sync>;

// ═══════════════════════════════════════════════════════════════
// Core types
// ═══════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellAction { Continue, Exit }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepKind { Client, Server, ServerCtx, Finalize }

/// Type-erased step stored in a Vec. NOT parameterized by Ctx so that
/// all steps share the same `Box<dyn ErasedStep>` Vec.
/// Contextual server steps receive `&dyn Any` and downcast to their Ctx type.
trait ErasedStep: Send + Sync {
    fn kind(&self) -> StepKind;

    fn client_exec(&self, input: &[u8], out: &mut dyn Console, input_src: &mut dyn InputSource) -> Result<Vec<u8>, String>;
    fn server_exec(&self, input: &[u8], ctx: &dyn Any) -> Result<Vec<u8>, String>;
}

// ═══════════════════════════════════════════════════════════════
// Concrete step implementations
// ═══════════════════════════════════════════════════════════════

struct ClientStepE<T, U, F> { closure: F, _ph: PhantomData<fn(T, U)> }

impl<T, U, F> ErasedStep for ClientStepE<T, U, F>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
    U: Serialize + DeserializeOwned + Send + Sync + 'static,
    F: Fn(T, &mut dyn Console, &mut dyn InputSource) -> Result<U, String> + Send + Sync + 'static,
{
    fn kind(&self) -> StepKind { StepKind::Client }
    fn client_exec(&self, input: &[u8], out: &mut dyn Console, input_src: &mut dyn InputSource) -> Result<Vec<u8>, String> {
        let data: T = serde_json::from_slice(input).map_err(|e| e.to_string())?;
        let result: U = (self.closure)(data, out, input_src)?;
        serde_json::to_vec(&result).map_err(|e| e.to_string())
    }
    fn server_exec(&self, _input: &[u8], _ctx: &dyn Any) -> Result<Vec<u8>, String> {
        unreachable!("server_exec called on client step")
    }
}

/// Stateless server step — closure takes only the request, ignores context.
struct ServerStepE<T, U, F> { closure: F, _ph: PhantomData<fn(T, U)> }

impl<T, U, F> ErasedStep for ServerStepE<T, U, F>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
    U: Serialize + DeserializeOwned + Send + Sync + 'static,
    F: Fn(T) -> Result<U, String> + Send + Sync + 'static,
{
    fn kind(&self) -> StepKind { StepKind::Server }
    fn client_exec(&self, _input: &[u8], _out: &mut dyn Console, _input_src: &mut dyn InputSource) -> Result<Vec<u8>, String> {
        unreachable!("client_exec called on server step")
    }
    fn server_exec(&self, input: &[u8], _ctx: &dyn Any) -> Result<Vec<u8>, String> {
        let data: T = serde_json::from_slice(input).map_err(|e| e.to_string())?;
        let result: U = (self.closure)(data)?;
        serde_json::to_vec(&result).map_err(|e| e.to_string())
    }
}

/// Contextual server step — closure takes request + shared `&Ctx`.
/// Downcasts `&dyn Any` to the concrete Ctx type at runtime.
struct ServerCtxStepE<T, U, F, Ctx> { closure: F, _ph: PhantomData<fn(T, U, Ctx)> }

impl<T, U, F, Ctx> ErasedStep for ServerCtxStepE<T, U, F, Ctx>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
    U: Serialize + DeserializeOwned + Send + Sync + 'static,
    F: Fn(T, &Ctx) -> Result<U, String> + Send + Sync + 'static,
    Ctx: 'static,
{
    fn kind(&self) -> StepKind { StepKind::ServerCtx }
    fn client_exec(&self, _input: &[u8], _out: &mut dyn Console, _input_src: &mut dyn InputSource) -> Result<Vec<u8>, String> {
        unreachable!("client_exec called on server step")
    }
    fn server_exec(&self, input: &[u8], ctx: &dyn Any) -> Result<Vec<u8>, String> {
        let data: T = serde_json::from_slice(input).map_err(|e| e.to_string())?;
        let ctx = ctx.downcast_ref::<Ctx>().ok_or("context type mismatch")?;
        let result: U = (self.closure)(data, ctx)?;
        serde_json::to_vec(&result).map_err(|e| e.to_string())
    }
}

struct FinalizeStepE<F> { closure: F }

impl<F> ErasedStep for FinalizeStepE<F>
where
    F: Fn() -> Result<ShellAction, String> + Send + Sync + 'static,
{
    fn kind(&self) -> StepKind { StepKind::Finalize }
    fn client_exec(&self, _input: &[u8], _out: &mut dyn Console, _input_src: &mut dyn InputSource) -> Result<Vec<u8>, String> {
        let _action = (self.closure)();
        Ok(Vec::new())
    }
    fn server_exec(&self, _input: &[u8], _ctx: &dyn Any) -> Result<Vec<u8>, String> {
        Ok(Vec::new())
    }
}

// ═══════════════════════════════════════════════════════════════
// Builder chain (type-state)
// ═══════════════════════════════════════════════════════════════

pub struct Plugin;

impl Plugin {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(name: &'static str, help: &'static str) -> ParseBuilder {
        ParseBuilder { name, help }
    }
}

pub struct ParseBuilder { name: &'static str, help: &'static str }

impl ParseBuilder {
    pub fn parse<T, F>(self, parse: F) -> Client<T>
    where
        T: Serialize + DeserializeOwned + Send + Sync + 'static,
        F: Fn(&str) -> Result<T, String> + Send + Sync + 'static,
    {
        let parse_bytes: ParseFn =
            Arc::new(move |s: &str| {
                let t: T = parse(s)?;
                serde_json::to_vec(&t).map_err(|e| e.to_string())
            });
        Client {
            name: self.name,
            help: self.help,
            parse: parse_bytes,
            steps: Vec::new(),
            _ph: PhantomData,
        }
    }
}

/// Client position. Can call `.client()` or `.finalize()`.
pub struct Client<T> {
    name: &'static str,
    help: &'static str,
    parse: ParseFn,
    steps: Vec<Box<dyn ErasedStep>>,
    _ph: PhantomData<T>,
}

pub type ClientHead<T> = Client<T>;
pub type ClientBuilder<T> = Client<T>;

impl<T> Client<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    /// CLIENT step: process T (with Console + InputSource), produce U.
    pub fn client<U, F>(mut self, f: F) -> Server<U>
    where
        U: Serialize + DeserializeOwned + Send + Sync + 'static,
        F: Fn(T, &mut dyn Console, &mut dyn InputSource) -> Result<U, String> + Send + Sync + 'static,
    {
        self.steps.push(Box::new(ClientStepE::<T, U, F> { closure: f, _ph: PhantomData }));
        Server {
            name: self.name, help: self.help, parse: self.parse,
            steps: self.steps, _ph: PhantomData,
        }
    }

    pub fn finalize<F>(mut self, f: F) -> Protocol
    where
        F: Fn() -> Result<ShellAction, String> + Send + Sync + 'static,
    {
        self.steps.push(Box::new(FinalizeStepE { closure: f }));
        Protocol {
            name: self.name, help: self.help,
            parse: self.parse, steps: self.steps,
            offline: None,
            completer: None,
        }
    }
}

/// Server position. Can call `.server()`, `.server_ctx()`, or `.finalize()`.
pub struct Server<T> {
    name: &'static str,
    help: &'static str,
    parse: ParseFn,
    steps: Vec<Box<dyn ErasedStep>>,
    _ph: PhantomData<T>,
}

pub type ServerBuilder<T> = Server<T>;

impl<T> Server<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    /// SERVER step (stateless): process T, produce U. No context access.
    pub fn server<U, F>(mut self, f: F) -> Client<U>
    where
        U: Serialize + DeserializeOwned + Send + Sync + 'static,
        F: Fn(T) -> Result<U, String> + Send + Sync + 'static,
    {
        self.steps.push(Box::new(ServerStepE::<T, U, F> { closure: f, _ph: PhantomData }));
        Client {
            name: self.name, help: self.help, parse: self.parse,
            steps: self.steps, _ph: PhantomData,
        }
    }

    /// SERVER step (stateful): process T with shared `&Ctx`, produce U.
    ///
    /// The Ctx type is inferred from the closure signature:
    /// `.server_ctx(|req, ctx: &DaemonState| ...)`
    pub fn server_ctx<U, Ctx, F>(mut self, f: F) -> Client<U>
    where
        U: Serialize + DeserializeOwned + Send + Sync + 'static,
        Ctx: 'static,
        F: Fn(T, &Ctx) -> Result<U, String> + Send + Sync + 'static,
    {
        self.steps.push(Box::new(ServerCtxStepE::<T, U, F, Ctx> { closure: f, _ph: PhantomData }));
        Client {
            name: self.name, help: self.help, parse: self.parse,
            steps: self.steps, _ph: PhantomData,
        }
    }

    pub fn finalize<F>(mut self, f: F) -> Protocol
    where
        F: Fn() -> Result<ShellAction, String> + Send + Sync + 'static,
    {
        self.steps.push(Box::new(FinalizeStepE { closure: f }));
        Protocol {
            name: self.name, help: self.help,
            parse: self.parse, steps: self.steps,
            offline: None,
            completer: None,
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Protocol — complete
// ═══════════════════════════════════════════════════════════════

/// Tab-completion function for the TUI input shell.
///
/// Receives the **confirmed** part of the current input line (never the
/// flashing, unconfirmed suggestion tail) and returns full-line suggestions.
/// The builtin command-name completion covers the first word; a completer
/// registered here is consulted once the command word is complete (i.e. the
/// input contains whitespace) and typically completes arguments.
pub type Completer = Arc<dyn Fn(&str) -> Vec<String> + Send + Sync>;

pub struct Protocol {
    pub name: &'static str,
    pub help: &'static str,
    parse: ParseFn,
    steps: Vec<Box<dyn ErasedStep>>,
    /// Optional fallback invoked when the server is unreachable (socket connect
    /// fails). Lets a command produce a local answer (e.g. `version` prints the
    /// client version) without a running server. If `None`, the connect error
    /// propagates as before.
    pub offline: Option<OfflineFn>,
    /// Optional tab-completion for argument positions (see [`Completer`]).
    pub(crate) completer: Option<Completer>,
}

impl Protocol {
    /// The registered tab-completion function, if any (see [`Self::complete`]).
    pub fn completer(&self) -> Option<&Completer> {
        self.completer.as_ref()
    }

    /// Register a tab-completion function.
    ///
    /// The function receives the whole **confirmed** input string (command
    /// name included) and returns full-line suggestions; the TUI shows them
    /// as an unconfirmed, flashing tail that Tab cycles through.
    pub fn complete<F>(mut self, f: F) -> Self
    where
        F: Fn(&str) -> Vec<String> + Send + Sync + 'static,
    {
        self.completer = Some(Arc::new(f));
        self
    }

    /// Register a fallback invoked when the server is unreachable.
    ///
    /// `execute_command` (TUI) and `run_cli_command` (CLI) catch a socket
    /// connect failure and, if a handler is registered, run it with the args
    /// and a console instead of returning an error. Lets commands such as
    /// `version` report a local answer when no server is running.
    pub fn offline<F>(mut self, f: F) -> Self
    where
        F: Fn(&str, &mut dyn Console) -> Result<(), String> + Send + Sync + 'static,
    {
        self.offline = Some(Arc::new(f));
        self
    }

    /// CLIENT side: parse args, walk steps, communicate with server.
    /// Returns the raw bytes of the last server response.
    pub fn run_client(
        &self,
        args: &str,
        conn: &mut dyn RawConnection,
        out: &mut dyn Console,
        input: &mut dyn InputSource,
    ) -> Result<Vec<u8>, String> {
        let mut data = (self.parse)(args)?;
        let mut last_server_response = Vec::new();

        for step in &self.steps {
            match step.kind() {
                StepKind::Client => {
                    let output = step.client_exec(&data, out, input)?;
                    conn.send_bytes(&output)?;
                    data = output;
                }
                StepKind::Server | StepKind::ServerCtx => {
                    data = conn.recv_bytes()?;
                    last_server_response = data.clone();
                    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&data) {
                        if let Some(err) = val.get("__error__").and_then(|v| v.as_str()) {
                            return Err(err.to_string());
                        }
                    }
                }
                StepKind::Finalize => {
                    let _action = step.client_exec(&data, out, input)?;
                    conn.send_typed(&())?;
                    return Ok(last_server_response);
                }
            }
        }
        conn.send_typed(&())?;
        Ok(last_server_response)
    }

    /// SERVER side: walk steps, communicate with client.
    /// `ctx` is shared server-side state passed to all `.server_ctx()` steps.
    /// On error: sends the error message back to client before closing.
    pub fn run_server(&self, conn: &mut dyn RawConnection, ctx: &dyn Any) -> Result<(), String> {
        let mut data = Vec::new();

        for step in &self.steps {
            match step.kind() {
                StepKind::Client => {
                    data = conn.recv_bytes()?;
                }
                StepKind::Server | StepKind::ServerCtx => {
                    match step.server_exec(&data, ctx) {
                        Ok(output) => {
                            conn.send_bytes(&output)?;
                            data = output;
                        }
                        Err(e) => {
                            let _ = conn.send_typed(&serde_json::json!({"__error__": e}));
                            return Err(e);
                        }
                    }
                }
                StepKind::Finalize => {
                    let _sentinel: () = conn.recv_typed()?;
                    return Ok(());
                }
            }
        }
        let _sentinel: () = conn.recv_typed()?;
        Ok(())
    }
}
