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

use std::marker::PhantomData;
use std::sync::Arc;

use serde::{de::DeserializeOwned, Serialize};

use crate::connection::{RawConnection, TypedConnection};
use crate::console::{Console, InputSource};

// ═══════════════════════════════════════════════════════════════
// Core types
// ═══════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellAction { Continue, Exit }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepKind { Client, Server, Finalize }

/// Type-erased step stored in a Vec. Each step handles its own ser/deser.
/// Input/output are raw bytes; the concrete step impl deserializes/serializes.
trait ErasedStep: Send + Sync {
    fn kind(&self) -> StepKind;

    /// Client-side: deserialize input, run closure (Console+InputSource), serialize output.
    fn client_exec(&self, input: &[u8], out: &mut dyn Console, input_src: &mut dyn InputSource) -> Result<Vec<u8>, String>;

    /// Server-side: deserialize input, run closure, serialize output.
    fn server_exec(&self, input: &[u8]) -> Result<Vec<u8>, String>;
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
    fn server_exec(&self, _input: &[u8]) -> Result<Vec<u8>, String> {
        unreachable!("server_exec called on client step")
    }
}

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
    fn server_exec(&self, input: &[u8]) -> Result<Vec<u8>, String> {
        let data: T = serde_json::from_slice(input).map_err(|e| e.to_string())?;
        let result: U = (self.closure)(data)?;
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
        let _action = (self.closure)()?;
        Ok(Vec::new())
    }
    fn server_exec(&self, _input: &[u8]) -> Result<Vec<u8>, String> {
        Ok(Vec::new())
    }
}

// ═══════════════════════════════════════════════════════════════
// Builder chain (type-state)
// ═══════════════════════════════════════════════════════════════

pub struct Plugin;

impl Plugin {
    pub fn new(name: &'static str, help: &'static str) -> ParseBuilder {
        ParseBuilder { name, help }
    }
}

pub struct ParseBuilder { name: &'static str, help: &'static str }

impl ParseBuilder {
    pub fn parse<T, F>(self, parse: F) -> ClientHead<T>
    where
        T: Serialize + DeserializeOwned + Send + Sync + 'static,
        F: Fn(&str) -> Result<T, String> + Send + Sync + 'static,
    {
        let parse_bytes: Arc<dyn Fn(&str) -> Result<Vec<u8>, String> + Send + Sync> =
            Arc::new(move |s: &str| {
                let t: T = parse(s)?;
                serde_json::to_vec(&t).map_err(|e| e.to_string())
            });
        ClientHead {
            name: self.name,
            help: self.help,
            parse: parse_bytes,
            steps: Vec::new(),
            _ph: PhantomData,
        }
    }
}

/// Client position. Can call .client() or .finalize().
pub struct Client<T> {
    name: &'static str,
    help: &'static str,
    parse: Arc<dyn Fn(&str) -> Result<Vec<u8>, String> + Send + Sync>,
    steps: Vec<Box<dyn ErasedStep>>,
    _ph: PhantomData<T>,
}

pub type ClientHead<T> = Client<T>;
pub type ClientBuilder<T> = Client<T>;

impl<T> Client<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
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
        }
    }
}

/// Server position. Can call .server() or .finalize().
pub struct Server<T> {
    name: &'static str,
    help: &'static str,
    parse: Arc<dyn Fn(&str) -> Result<Vec<u8>, String> + Send + Sync>,
    steps: Vec<Box<dyn ErasedStep>>,
    _ph: PhantomData<T>,
}

pub type ServerBuilder<T> = Server<T>;

impl<T> Server<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
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

    pub fn finalize<F>(mut self, f: F) -> Protocol
    where
        F: Fn() -> Result<ShellAction, String> + Send + Sync + 'static,
    {
        self.steps.push(Box::new(FinalizeStepE { closure: f }));
        Protocol {
            name: self.name, help: self.help,
            parse: self.parse, steps: self.steps,
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Protocol — complete
// ═══════════════════════════════════════════════════════════════

pub struct Protocol {
    pub name: &'static str,
    pub help: &'static str,
    parse: Arc<dyn Fn(&str) -> Result<Vec<u8>, String> + Send + Sync>,
    steps: Vec<Box<dyn ErasedStep>>,
}

impl Protocol {
    /// CLIENT side: parse args, walk steps, communicate with server.
    pub fn run_client(
        &self,
        args: &str,
        conn: &mut dyn RawConnection,
        out: &mut dyn Console,
        input: &mut dyn InputSource,
    ) -> Result<(), String> {
        // 1. Parse → initial data bytes
        let mut data = (self.parse)(args)?;

        // 2. Walk steps
        for step in &self.steps {
            match step.kind() {
                StepKind::Client => {
                    // Process data in-process, send result to server
                    let output = step.client_exec(&data, out, input)?;
                    conn.send_bytes(&output)?;
                    data = output; // keep for potential next client step
                }
                StepKind::Server => {
                    // Wait for server's response — may be data or an error
                    data = conn.recv_bytes()?;
                    // Check if server sent an error
                    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&data) {
                        if let Some(err) = val.get("__error__").and_then(|v| v.as_str()) {
                            return Err(err.to_string());
                        }
                    }
                }
                StepKind::Finalize => {
                    // Run finalize closure
                    let _action = step.client_exec(&data, out, input)?;
                    // Send sentinel
                    conn.send_typed(&())?;
                    return Ok(());
                }
            }
        }
        // If no finalize step, send sentinel anyway
        conn.send_typed(&())?;
        Ok(())
    }

    /// SERVER side: walk steps, communicate with client.
    /// On error: sends the error message back to client before closing.
    pub fn run_server(&self, conn: &mut dyn RawConnection) -> Result<(), String> {
        let mut data = Vec::new();

        for step in &self.steps {
            match step.kind() {
                StepKind::Client => {
                    data = conn.recv_bytes()?;
                }
                StepKind::Server => {
                    match step.server_exec(&data) {
                        Ok(output) => {
                            conn.send_bytes(&output)?;
                            data = output;
                        }
                        Err(e) => {
                            // Send error back to client
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
