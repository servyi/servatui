//! Builder chain: Plugin → Client<T> ⇄ Server<T> → Protocol
//!
//! The type alternates Client → Server → Client → Server → ...
//! You cannot call .client() on Server or .server() on Client.
//! The Rust compiler enforces the alternation.

use std::marker::PhantomData;
use std::sync::Arc;

use serde::{de::DeserializeOwned, Serialize};

use crate::connection::{RawConnection, TypedConnection};
use crate::console::{Console, InputSource};

// ═══════════════════════════════════════════════════════════════
// Core traits
// ═══════════════════════════════════════════════════════════════

/// What a plugin returns after the conversation completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellAction {
    Continue,
    Exit,
}

/// Non-generic trait (erased via dyn). Each concrete step implementation
/// handles its own serialization/deserialization internally via Connection.
pub trait ProtocolStep<Ctx: Send, ClientCtx: Send>: Send + Sync {
    fn client(
        &self,
        cctx: &mut ClientCtx,
        conn: &mut dyn RawConnection,
        out: &mut dyn Console,
        input: &mut dyn InputSource,
    ) -> Result<(), String>;

    fn server(&self, ctx: &mut Ctx, conn: &mut dyn RawConnection) -> Result<(), String>;
}

// ═══════════════════════════════════════════════════════════════
// Concrete step implementations
// ═══════════════════════════════════════════════════════════════

/// A client-side step: receives T from wire, runs closure, sends U to wire.
/// Created by `Client<T>.client::<U>(closure)`.
pub struct StepClient<Ctx: Send, ClientCtx: Send, T, U, F, Next> {
    pub closure: F,
    pub next: Next,
    _ph: PhantomData<fn(T, U, Ctx, ClientCtx)>,
}

/// A server-side step: receives T from wire, runs closure with Ctx, sends U.
/// Created by `Server<T>.server::<U>(closure)`.
pub struct StepServer<Ctx: Send, ClientCtx: Send, T, U, F, Next> {
    pub closure: F,
    pub next: Next,
    _ph: PhantomData<fn(T, U, Ctx, ClientCtx)>,
}

/// Terminal node: receives T from wire, runs finalize closure.
/// Created by `Client<T>.finalize(closure)` or `Server<T>.finalize(closure)`.
pub struct StepFinalize<Ctx: Send, ClientCtx: Send, T, F> {
    pub closure: F,
    _ph: PhantomData<fn(T, Ctx, ClientCtx)>,
}

// ── ProtocolStep impls ──────────────────────────────────────────

impl<Ctx, ClientCtx, T, U, F, Next> ProtocolStep<Ctx, ClientCtx>
    for StepClient<Ctx, ClientCtx, T, U, F, Next>
where
    Ctx: Send + Sync,
    ClientCtx: Send + Sync,
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
    U: Serialize + DeserializeOwned + Send + Sync + 'static,
    F: Fn(T, &mut ClientCtx, &mut dyn Console, &mut dyn InputSource) -> Result<U, String>
        + Send
        + Sync,
    Next: ProtocolStep<Ctx, ClientCtx>,
{
    fn client(
        &self,
        cctx: &mut ClientCtx,
        conn: &mut dyn RawConnection,
        out: &mut dyn Console,
        input: &mut dyn InputSource,
    ) -> Result<(), String> {
        let data: T = conn.recv_typed()?;
        let result: U = (self.closure)(data, cctx, out, input)?;
        conn.send_typed(&result)?;
        self.next.client(cctx, conn, out, input)
    }

    fn server(&self, ctx: &mut Ctx, conn: &mut dyn RawConnection) -> Result<(), String> {
        // Client step on server: receive U from wire, pass to next.
        let _data: U = conn.recv_typed()?;
        self.next.server(ctx, conn)
    }
}

impl<Ctx, ClientCtx, T, U, F, Next> ProtocolStep<Ctx, ClientCtx>
    for StepServer<Ctx, ClientCtx, T, U, F, Next>
where
    Ctx: Send + Sync,
    ClientCtx: Send + Sync,
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
    U: Serialize + DeserializeOwned + Send + Sync + 'static,
    F: Fn(T, &mut Ctx) -> Result<U, String> + Send + Sync,
    Next: ProtocolStep<Ctx, ClientCtx>,
{
    fn client(
        &self,
        cctx: &mut ClientCtx,
        conn: &mut dyn RawConnection,
        out: &mut dyn Console,
        input: &mut dyn InputSource,
    ) -> Result<(), String> {
        // Server step on client: receive U from wire, pass to next.
        let _data: U = conn.recv_typed()?;
        self.next.client(cctx, conn, out, input)
    }

    fn server(&self, ctx: &mut Ctx, conn: &mut dyn RawConnection) -> Result<(), String> {
        let data: T = conn.recv_typed()?;
        let result: U = (self.closure)(data, ctx)?;
        conn.send_typed(&result)?;
        self.next.server(ctx, conn)
    }
}

impl<Ctx, ClientCtx, T, F> ProtocolStep<Ctx, ClientCtx> for StepFinalize<Ctx, ClientCtx, T, F>
where
    Ctx: Send + Sync,
    ClientCtx: Send + Sync,
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
    F: Fn(&mut ClientCtx) -> Result<ShellAction, String> + Send + Sync,
{
    fn client(
        &self,
        cctx: &mut ClientCtx,
        conn: &mut dyn RawConnection,
        _out: &mut dyn Console,
        _input: &mut dyn InputSource,
    ) -> Result<(), String> {
        // Finalize: the T was consumed by the previous step's closure.
        // We receive a sentinel () and produce the ShellAction.
        let _sentinel: () = conn.recv_typed()?;
        let _action = (self.closure)(cctx)?;
        Ok(())
    }

    fn server(&self, _ctx: &mut Ctx, conn: &mut dyn RawConnection) -> Result<(), String> {
        // Server side: wait for client's sentinel, then done.
        let _sentinel: () = conn.recv_typed()?;
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════
// Builder types
// ═══════════════════════════════════════════════════════════════

/// Entry point for defining a protocol.
pub struct Plugin;

impl Plugin {
    /// Create a new protocol with the given command name and help text.
    pub fn new(name: &'static str, help: &'static str) -> ParseBuilder {
        ParseBuilder { name, help }
    }
}

/// Intermediate builder: holds name + help, waiting for parse closure.
pub struct ParseBuilder {
    name: &'static str,
    help: &'static str,
}

impl ParseBuilder {
    /// Provide the parser closure: &str → T.
    /// Returns Client<T> — the start of the alternation chain.
    pub fn parse<T, F>(self, parse: F) -> ClientHead<T>
    where
        T: Serialize + DeserializeOwned + Send + Sync + 'static,
        F: Fn(&str) -> Result<T, String> + Send + Sync + 'static,
    {
        ClientHead {
            name: self.name,
            help: self.help,
            parse: Arc::new(parse),
            _ph: PhantomData,
        }
    }
}

/// Starting point of the chain — holds the parse closure.
/// Client<T> can call .client() or .finalize().
pub struct ClientHead<T> {
    name: &'static str,
    help: &'static str,
    parse: Arc<dyn Fn(&str) -> Result<T, String> + Send + Sync>,
    _ph: PhantomData<T>,
}

impl<T> ClientHead<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    /// Client-side step: T → U.
    /// Closure receives T, ClientCtx, Console, InputSource and produces U.
    /// Result U is serialized and sent to the server.
    /// Returns Server<U> — you must now call .server() or .finalize().
    pub fn client<U, F>(
        self,
        f: F,
    ) -> ServerBuilder<
        StepClient<(), (), T, U, F, ParseStep<T>>,
    >
    where
        U: Serialize + DeserializeOwned + Send + Sync + 'static,
        F: Fn(T, &mut (), &mut dyn Console, &mut dyn InputSource) -> Result<U, String>
            + Send
            + Sync
            + 'static,
    {
        // The parse step is the head; this client step follows.
        let head = ParseStep {
            parse: self.parse.clone(),
            _ph: PhantomData,
        };
        let step = StepClient {
            closure: f,
            next: head,
            _ph: PhantomData,
        };
        ServerBuilder {
            name: self.name,
            help: self.help,
            step,
        }
    }
}

/// A client-side step in the chain. Type parameter T is the current data.
/// Can call .client() to do another client step, or .finalize().
///
/// Note: you reach this after a Server<T> step.
pub struct ClientBuilder<Step> {
    name: &'static str,
    help: &'static str,
    step: Step,
}

/// A server-side step in the chain. Type parameter T is the current data.
/// Can call .server() to do another server step, or .finalize().
pub struct ServerBuilder<Step> {
    name: &'static str,
    help: &'static str,
    step: Step,
}

impl<Ctx, ClientCtx, T, U, F, Next> ServerBuilder<StepClient<Ctx, ClientCtx, T, U, F, Next>>
where
    Ctx: Send + Sync + 'static,
    ClientCtx: Send + Sync + 'static,
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
    U: Serialize + DeserializeOwned + Send + Sync + 'static,
    F: Fn(T, &mut ClientCtx, &mut dyn Console, &mut dyn InputSource) -> Result<U, String>
        + Send
        + Sync
        + 'static,
    Next: ProtocolStep<Ctx, ClientCtx>,
{
    /// Server-side step: receive U from wire, run closure with &mut Ctx, send V.
    /// Returns ClientBuilder — you must now call .client() or .finalize().
    pub fn server<V, G>(
        self,
        g: G,
    ) -> ClientBuilder<StepServer<Ctx, ClientCtx, U, V, G, StepClient<Ctx, ClientCtx, T, U, F, Next>>>
    where
        V: Serialize + DeserializeOwned + Send + Sync + 'static,
        G: Fn(U, &mut Ctx) -> Result<V, String> + Send + Sync + 'static,
    {
        let step = StepServer {
            closure: g,
            next: self.step,
            _ph: PhantomData,
        };
        ClientBuilder {
            name: self.name,
            help: self.help,
            step,
        }
    }

    /// Finalize from a Server builder position.
    /// Note: you can only finalize from a Client position (after the server
    /// step sends data back). If you're at a Server builder, the previous
    /// Client step's data is already on the wire — finalize here doesn't
    /// make sense. Use .server() first.
    pub fn finalize(self) -> Protocol {
        // This is a terminal state that shouldn't normally be reached.
        // The chain should end with Client.finalize().
        // But we provide it for flexibility.
        Protocol {
            name: self.name,
            help: self.help,
        }
    }
}

impl<Ctx, ClientCtx, T, U, F, Next> ClientBuilder<StepServer<Ctx, ClientCtx, T, U, F, Next>>
where
    Ctx: Send + Sync + 'static,
    ClientCtx: Send + Sync + 'static,
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
    U: Serialize + DeserializeOwned + Send + Sync + 'static,
    F: Fn(T, &mut Ctx) -> Result<U, String> + Send + Sync + 'static,
    Next: ProtocolStep<Ctx, ClientCtx>,
{
    /// Client-side step: receive U from wire, run closure, send V.
    /// Returns ServerBuilder — you must now call .server() or .finalize().
    pub fn client<V, G>(
        self,
        g: G,
    ) -> ServerBuilder<StepClient<Ctx, ClientCtx, U, V, G, StepServer<Ctx, ClientCtx, T, U, F, Next>>>
    where
        V: Serialize + DeserializeOwned + Send + Sync + 'static,
        G: Fn(U, &mut ClientCtx, &mut dyn Console, &mut dyn InputSource) -> Result<V, String>
            + Send
            + Sync
            + 'static,
    {
        let step = StepClient {
            closure: g,
            next: self.step,
            _ph: PhantomData,
        };
        ServerBuilder {
            name: self.name,
            help: self.help,
            step,
        }
    }

    /// Finalize: terminal node. Receives sentinel, produces ShellAction.
    pub fn finalize<H>(self, _h: H) -> Protocol
    where
        H: Fn(&mut ClientCtx) -> Result<ShellAction, String> + Send + Sync + 'static,
    {
        Protocol {
            name: self.name,
            help: self.help,
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Parse step — the head of the chain
// ═══════════════════════════════════════════════════════════════

/// The head of the linked list: holds the parse closure.
/// On the client side, it parses args and sends T.
/// On the server side, it receives T and passes to the next step.
pub struct ParseStep<T> {
    parse: Arc<dyn Fn(&str) -> Result<T, String> + Send + Sync>,
    _ph: PhantomData<T>,
}

impl<Ctx: Send + Sync, ClientCtx: Send + Sync, T> ProtocolStep<Ctx, ClientCtx> for ParseStep<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn client(
        &self,
        _cctx: &mut ClientCtx,
        _conn: &mut dyn RawConnection,
        _out: &mut dyn Console,
        _input: &mut dyn InputSource,
    ) -> Result<(), String> {
        // ParseStep is the terminal of the reversed linked list.
        // The actual parsing happens in the entry point, not here.
        Ok(())
    }

    fn server(&self, _ctx: &mut Ctx, _conn: &mut dyn RawConnection) -> Result<(), String> {
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════
// Protocol — complete, ready to register
// ═══════════════════════════════════════════════════════════════

/// A complete protocol definition.
pub struct Protocol {
    pub name: &'static str,
    pub help: &'static str,
}
