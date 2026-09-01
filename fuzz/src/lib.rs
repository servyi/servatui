//! Shared fixtures and case runners for the servatui fuzz targets.
//!
//! Philosophy: we trust the client, the server, and the user. What these
//! harnesses exercise are rare but *legitimate* paths — unusual-but-valid
//! wire values, handler failures, disconnects at each conversation point —
//! and check that the framework upholds its contract on every one of them:
//!
//! * no panics, no hangs;
//! * every server step answers with exactly one frame of valid JSON;
//! * a failing handler step produces exactly one `{"__error__": msg}` frame
//!   and the same `msg` in the `Err` return (client sees it verbatim);
//! * a disconnect surfaces as `Err` on the side that reads past it;
//! * the client sends exactly the frames its steps produce, and renders
//!   exactly the lines its render steps compute.
//!
//! Everything a case does is a deterministic function of its [`CaseSpec`],
//! and every fixture's protocol closures have a *mirror* here that computes
//! the expected wire traffic / rendering from the same spec. A mismatch
//! between the framework and its mirror is a finding.

use std::any::Any;
use std::collections::VecDeque;
use std::sync::Mutex;

use arbitrary::Arbitrary;
use serde::{Deserialize, Serialize};
use servyi_servatui::connection::TestEndpoint;
use servyi_servatui::console::{BufferConsole, NoInput};
use servyi_servatui::protocol::Protocol;
use servyi_servatui::server::ServerHandle;
use servyi_servatui::{Plugin, ShellAction, SocketConnection};

// ---------------------------------------------------------------------------
// Value pool
// ---------------------------------------------------------------------------

/// Rare-but-reasonable strings boosted into the argument pool.
fn probes() -> Vec<String> {
    vec![
        String::new(),
        " ".into(),
        "héllo".into(),
        "日本語".into(),
        "↪ continuation".into(),
        "a\nb".into(),
        "\t".into(),
        "\u{1F600} ok".into(),
        "lineachar".into(),
        "-".repeat(200),
    ]
}

/// Boost short/empty drawn strings into one of the rare-but-reasonable probes.
fn sprout(s: &str, salt: u8) -> String {
    if s.chars().count() < 2 {
        let probes = probes();
        probes[salt as usize % probes.len()].clone()
    } else {
        s.to_string()
    }
}

// ---------------------------------------------------------------------------
// Fixture wire types
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EchoArgs {
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EchoResult {
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Query {
    pub text: String,
    pub n: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Answer {
    pub n: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Confirm {
    pub ok: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Done {
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CtxRequest {
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CtxResponse {
    pub total: u64,
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BoomArgs {
    pub trigger: u8,
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BoomResult {
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum RichEnum {
    Empty,
    Word(String),
    Pair { k: i64 },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RichArgs {
    pub opt: Option<String>,
    pub e: RichEnum,
    pub v: Vec<i64>,
    pub big: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RichResult {
    pub echo: String,
    pub count: u64,
}

/// Shared server-side state for the `ctx` fixture.
#[derive(Default)]
pub struct CtxState {
    pub entries: Mutex<Vec<String>>,
}

impl CtxState {
    pub fn new(pre_seed: usize) -> Self {
        Self { entries: Mutex::new((0..pre_seed).map(|i| format!("pre{i}")).collect()) }
    }

    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}

// ---------------------------------------------------------------------------
// Fixture protocols
// ---------------------------------------------------------------------------

#[derive(Arbitrary, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureId {
    Ping,
    Echo,
    Multi,
    Ctx,
    Boom,
    Rich,
}

impl FixtureId {
    pub fn name(self) -> &'static str {
        match self {
            FixtureId::Ping => "ping",
            FixtureId::Echo => "echo",
            FixtureId::Multi => "multi",
            FixtureId::Ctx => "ctx",
            FixtureId::Boom => "boom",
            FixtureId::Rich => "rich",
        }
    }

    /// The protocol under this fixture. Closures here are the system under
    /// test; their behavior is mirrored by [`CaseSpec`].
    pub fn protocol(self) -> Protocol {
        match self {
            FixtureId::Ping => Plugin::new("ping", "fuzz ping")
                .parse(|_args: &str| Ok(()))
                .client(|_: (), _out, _input| Ok(()))
                .server(|_: ()| Ok(EchoResult { text: "pong".into() }))
                .client(|r: EchoResult, out, _input| {
                    out.print_line(&r.text);
                    Ok(())
                })
                .finalize(|| Ok(ShellAction::Continue)),
            FixtureId::Echo => Plugin::new("echo", "fuzz echo")
                .parse(|args: &str| Ok(EchoArgs { text: args.to_string() }))
                .client(|a: EchoArgs, _out, _input| Ok(a))
                .server(|a: EchoArgs| Ok(EchoResult { text: a.text }))
                .client(|r: EchoResult, out, _input| {
                    out.print_line(&r.text);
                    Ok(())
                })
                .finalize(|| Ok(ShellAction::Continue)),
            FixtureId::Multi => Plugin::new("multi", "fuzz multi")
                .parse(|args: &str| {
                    let t = args.trim();
                    if t.is_empty() {
                        Err("empty query".into())
                    } else {
                        Ok(Query { text: t.to_string(), n: args.len() as u64 })
                    }
                })
                .client(|q: Query, _out, _input| Ok(q))
                .server(|q: Query| Ok(Answer { n: q.n }))
                .client(|a: Answer, out, _input| {
                    out.print_line(&format!("a={}", a.n));
                    Ok(Confirm { ok: true })
                })
                .server(|c: Confirm| {
                    Ok(Done { text: if c.ok { "done".into() } else { "not-done".into() } })
                })
                .client(|d: Done, out, _input| {
                    out.print_line(&d.text);
                    Ok(())
                })
                .finalize(|| Ok(ShellAction::Continue)),
            FixtureId::Ctx => Plugin::new("ctx", "fuzz ctx")
                .parse(|args: &str| Ok(CtxRequest { text: args.to_string() }))
                .client(|r: CtxRequest, _out, _input| Ok(r))
                .server_ctx(|r: CtxRequest, ctx: &CtxState| {
                    let mut entries = ctx.entries.lock().unwrap();
                    entries.push(r.text.clone());
                    Ok(CtxResponse { total: entries.len() as u64, text: r.text })
                })
                .client(|r: CtxResponse, out, _input| {
                    out.print_line(&format!("total={} {}", r.total, r.text));
                    Ok(())
                })
                .finalize(|| Ok(ShellAction::Continue)),
            FixtureId::Boom => Plugin::new("boom", "fuzz boom")
                .parse(|args: &str| {
                    let trigger = if args.starts_with('!') { 1 } else { 0 };
                    let text = args.strip_prefix('!').unwrap_or(args);
                    Ok(BoomArgs { trigger, text: text.to_string() })
                })
                .client(|b: BoomArgs, _out, _input| Ok(b))
                .server(|b: BoomArgs| {
                    if b.trigger == 1 {
                        Err(format!("boom: {}", b.text))
                    } else {
                        Ok(BoomResult { text: b.text })
                    }
                })
                .client(|r: BoomResult, out, _input| {
                    out.print_line(&r.text);
                    Ok(())
                })
                .finalize(|| Ok(ShellAction::Continue)),
            FixtureId::Rich => Plugin::new("rich", "fuzz rich")
                .parse(|args: &str| {
                    let e = if args.is_empty() {
                        RichEnum::Empty
                    } else if args.starts_with('|') {
                        RichEnum::Pair { k: args.len() as i64 }
                    } else {
                        RichEnum::Word(args.to_string())
                    };
                    Ok(RichArgs {
                        opt: if args.is_empty() { None } else { Some(args.to_string()) },
                        e,
                        v: args.split(',').map(|s| s.len() as i64).collect(),
                        big: args.len() as u64,
                    })
                })
                .client(|a: RichArgs, _out, _input| Ok(a))
                .server(|a: RichArgs| {
                    let echo = a.opt.unwrap_or_else(|| match &a.e {
                        RichEnum::Word(w) => w.clone(),
                        _ => String::new(),
                    });
                    Ok(RichResult { echo, count: a.v.len() as u64 + a.big % 7 })
                })
                .client(|r: RichResult, out, _input| {
                    out.print_line(&format!("{}|{}", r.echo, r.count));
                    Ok(())
                })
                .finalize(|| Ok(ShellAction::Continue)),
        }
    }

    /// All fixtures, registered under their command names.
    pub fn all_protocols() -> Vec<Protocol> {
        use FixtureId::*;
        vec![Ping.protocol(), Echo.protocol(), Multi.protocol(), Ctx.protocol(), Boom.protocol(), Rich.protocol()]
    }

    /// Number of server steps (= response frames on a successful run).
    pub fn server_steps(self) -> usize {
        match self {
            FixtureId::Multi => 2,
            _ => 1,
        }
    }

    /// Client→server frames a well-behaved client sends, in order
    /// (parse output, mid-step outputs, render output, sentinel).
    pub fn client_frames(self, spec: &CaseSpec) -> Vec<Vec<u8>> {
        let null = b"null".to_vec();
        match self {
            FixtureId::Ping => vec![null.clone(), null.clone(), null],
            FixtureId::Echo => {
                vec![ser(&EchoArgs { text: spec.args().clone() }), null.clone(), null]
            }
            FixtureId::Multi => vec![
                ser(&Query { text: spec.args().trim().to_string(), n: spec.args().len() as u64 }),
                ser(&Confirm { ok: true }),
                null.clone(),
                null,
            ],
            FixtureId::Ctx => {
                vec![ser(&CtxRequest { text: spec.args().clone() }), null.clone(), null]
            }
            FixtureId::Boom => {
                vec![ser(&spec.boom_args()), null.clone(), null]
            }
            FixtureId::Rich => {
                vec![ser(&spec.rich_args()), null.clone(), null]
            }
        }
    }

    /// Server→client frames on a successful run, in order.
    pub fn server_frames(self, spec: &CaseSpec) -> Vec<Vec<u8>> {
        match self {
            FixtureId::Ping => vec![ser(&EchoResult { text: "pong".into() })],
            FixtureId::Echo => vec![ser(&EchoResult { text: spec.args().clone() })],
            FixtureId::Multi => vec![
                ser(&Answer { n: spec.args().len() as u64 }),
                ser(&Done { text: "done".into() }),
            ],
            FixtureId::Ctx => vec![ser(&CtxResponse {
                total: spec.pre_seed as u64 + 1,
                text: spec.args().clone(),
            })],
            FixtureId::Boom => vec![ser(&BoomResult { text: spec.boom_args().text })],
            FixtureId::Rich => {
                let a = spec.rich_args();
                let echo = a.opt.unwrap_or_else(|| match &a.e {
                    RichEnum::Word(w) => w.clone(),
                    _ => String::new(),
                });
                vec![ser(&RichResult { echo, count: a.v.len() as u64 + a.big % 7 })]
            }
        }
    }

    /// Console lines the client's render steps print on a successful run.
    pub fn expected_render(self, spec: &CaseSpec) -> Vec<String> {
        match self {
            FixtureId::Ping => vec!["pong".into()],
            FixtureId::Echo => vec![spec.args().clone()],
            FixtureId::Multi => vec![format!("a={}", spec.args().len()), "done".into()],
            FixtureId::Ctx => vec![format!("total={} {}", spec.pre_seed as usize + 1, spec.args())],
            FixtureId::Boom => vec![spec.boom_args().text],
            FixtureId::Rich => {
                let a = spec.rich_args();
                let echo = a.opt.unwrap_or_else(|| match &a.e {
                    RichEnum::Word(w) => w.clone(),
                    _ => String::new(),
                });
                vec![format!("{}|{}", echo, a.v.len() as u64 + a.big % 7)]
            }
        }
    }

    /// Console lines printed once the first `k` server responses arrived
    /// (multi renders between its two responses; everything else renders
    /// after its single response).
    pub fn expected_render_prefix(self, spec: &CaseSpec, responses_received: usize) -> Vec<String> {
        match self {
            FixtureId::Multi if responses_received >= 1 => vec![format!("a={}", spec.args().len())],
            _ => Vec::new(),
        }
    }

    /// Client send/recv order by step position: S = client sends a frame,
    /// R = client waits for a server response.
    pub fn io_layout(self) -> &'static [Io] {
        match self {
            FixtureId::Multi => &[Io::S, Io::R, Io::S, Io::R, Io::S, Io::S],
            _ => &[Io::S, Io::R, Io::S, Io::S],
        }
    }

    /// Server steps that have run once the client fed `fed` conversation
    /// frames (0 = none). A server step runs when its input frame arrived.
    pub fn server_steps_after(self, fed: usize) -> usize {
        match self {
            FixtureId::Multi => (fed >= 1) as usize + (fed >= 2) as usize,
            _ => (fed >= 1) as usize,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Io {
    S,
    R,
}

// ---------------------------------------------------------------------------
// Case spec — the single source of truth every mirror derives from
// ---------------------------------------------------------------------------

/// A fully determined conversation scenario.
#[derive(Debug, Clone)]
pub struct CaseSpec {
    pub fixture: FixtureId,
    /// Raw CLI args before probe-boosting / fail-prefixing.
    pub args_raw: String,
    /// Salt choosing which rare-but-reasonable probe to boost into `args`.
    pub salt: u8,
    /// Pre-existing entries in the `ctx` fixture's shared state.
    pub pre_seed: u8,
    /// Make the `boom` fixture's server step fail (canonicalized into a
    /// leading `!` in the args, exactly like a real client would).
    pub fail: bool,
}

impl CaseSpec {
    /// Final args after boosting short strings into rare-but-reasonable
    /// probes and canonicalizing the boom trigger.
    pub fn args(&self) -> String {
        let base = sprout(&self.args_raw, self.salt);
        if self.fail && self.fixture == FixtureId::Boom {
            format!("!{base}")
        } else {
            base
        }
    }

    /// The `multi` fixture's parse error, if the args cannot be parsed.
    pub fn parse_error(&self) -> Option<String> {
        if self.fixture == FixtureId::Multi && self.args().trim().is_empty() {
            Some("empty query".into())
        } else {
            None
        }
    }

    /// The error a failing handler step produces (boom only).
    pub fn server_error(&self) -> Option<String> {
        if self.fixture == FixtureId::Boom && self.args().starts_with('!') {
            Some(format!("boom: {}", self.boom_args().text))
        } else {
            None
        }
    }

    pub fn boom_args(&self) -> BoomArgs {
        let args = self.args();
        let trigger = if args.starts_with('!') { 1 } else { 0 };
        let text = args.strip_prefix('!').unwrap_or(&args);
        BoomArgs { trigger, text: text.to_string() }
    }

    pub fn rich_args(&self) -> RichArgs {
        let args = self.args();
        let e = if args.is_empty() {
            RichEnum::Empty
        } else if args.starts_with('|') {
            RichEnum::Pair { k: args.len() as i64 }
        } else {
            RichEnum::Word(args.clone())
        };
        RichArgs {
            opt: if args.is_empty() { None } else { Some(args.clone()) },
            e,
            v: args.split(',').map(|s| s.len() as i64).collect(),
            big: args.len() as u64,
        }
    }

    /// Convenience views mirroring [`FixtureId`] methods.
    pub fn client_frames(&self) -> Vec<Vec<u8>> {
        self.fixture.client_frames(self)
    }

    pub fn server_frames(&self) -> Vec<Vec<u8>> {
        self.fixture.server_frames(self)
    }

    pub fn expected_render(&self) -> Vec<String> {
        self.fixture.expected_render(self)
    }
}

// ---------------------------------------------------------------------------
// Harness plumbing
// ---------------------------------------------------------------------------

fn ser<T: Serialize>(v: &T) -> Vec<u8> {
    serde_json::to_vec(v).expect("fixture values are always serializable")
}

fn endpoint_with(incoming: Vec<Vec<u8>>) -> TestEndpoint {
    TestEndpoint {
        incoming: Mutex::new(incoming.into()),
        outgoing: Mutex::new(VecDeque::new()),
    }
}

fn outgoing_frames(ep: &TestEndpoint) -> Vec<Vec<u8>> {
    ep.outgoing.lock().unwrap().iter().cloned().collect()
}

fn parse_json(frame: &[u8], what: &str) -> serde_json::Value {
    serde_json::from_slice(frame).unwrap_or_else(|e| panic!("{what} is not valid JSON: {e}"))
}

/// Assert that two wire frames carry the same JSON value.
fn assert_frame_eq(actual: &[u8], expected: &[u8], what: &str) {
    assert_eq!(
        parse_json(actual, what),
        parse_json(expected, what),
        "{what}: frame mismatch\n actual:   {}\n expected: {}",
        String::from_utf8_lossy(actual),
        String::from_utf8_lossy(expected),
    );
}

// ---------------------------------------------------------------------------
// T1 — server side: legitimate client conversations and disconnects
// ---------------------------------------------------------------------------

#[derive(Arbitrary, Debug, Clone)]
pub struct ServerCase {
    pub fixture: FixtureId,
    pub args_raw: String,
    pub salt: u8,
    pub pre_seed: u8,
    /// 0 = feed no conversation frames; capped at the full conversation.
    pub feed: u8,
    /// Make the boom server step fail.
    pub fail: bool,
    /// Route through `handle_connection` (command dispatch) instead of
    /// calling `Protocol::run_server` directly.
    pub via_dispatch: bool,
    /// Dispatch under an unregistered command name (client/server skew).
    pub unknown_cmd: bool,
}

impl ServerCase {
    fn spec(&self) -> CaseSpec {
        CaseSpec {
            fixture: self.fixture,
            args_raw: self.args_raw.clone(),
            salt: self.salt,
            pre_seed: self.pre_seed,
            fail: self.fail,
        }
    }
}

/// Drive the server side of one conversation and check the contract.
pub fn run_server_case(c: &ServerCase) {
    let spec = c.spec();
    // A client whose parse fails never opens the conversation.
    if spec.parse_error().is_some() {
        return;
    }

    let frames = spec.client_frames();
    let fed = (c.feed as usize).min(frames.len());
    let via_dispatch = c.via_dispatch || c.unknown_cmd;

    let mut incoming: Vec<Vec<u8>> = Vec::new();
    if via_dispatch {
        let cmd: String = if c.unknown_cmd {
            "nope".to_string()
        } else {
            spec.fixture.name().to_string()
        };
        incoming.push(ser(&cmd));
    }
    incoming.extend(frames[..fed].iter().cloned());

    let mut ep = endpoint_with(incoming);
    let ctx = CtxState::new(c.pre_seed as usize);
    let ctx_any: &dyn Any = &ctx;

    let result = if via_dispatch {
        let protocols = FixtureId::all_protocols();
        ServerHandle::handle_connection(&protocols, &mut ep, ctx_any)
    } else {
        spec.fixture.protocol().run_server(&mut ep, ctx_any)
    };

    let out = outgoing_frames(&ep);

    if c.unknown_cmd {
        let err = result.expect_err("unknown command must be an error");
        assert!(
            err.contains("Unknown command"),
            "unknown command: unexpected error text: {err}"
        );
        assert!(out.is_empty(), "server must not respond to an unknown command");
        assert_eq!(
            ep.incoming.lock().unwrap().len(),
            fed,
            "only the command line should have been consumed"
        );
        return;
    }

    match result {
        Ok(()) => {
            assert!(spec.server_error().is_none(), "Ok(()) despite failing handler");
            assert_eq!(fed, frames.len(), "Ok(()) with a truncated feed");
            assert_eq!(
                out.len(),
                spec.fixture.server_steps(),
                "each server step must answer exactly one frame"
            );
            let expected = spec.server_frames();
            for (i, frame) in out.iter().enumerate() {
                assert_frame_eq(frame, &expected[i], &format!("response frame {i}"));
            }
            assert!(
                ep.incoming.lock().unwrap().is_empty(),
                "server left conversation frames unconsumed"
            );
            if spec.fixture == FixtureId::Ctx {
                assert_eq!(
                    ctx.len(),
                    c.pre_seed as usize + 1,
                    "ctx step must append exactly one entry"
                );
            }
        }
        Err(e) => {
            if let Some(want) = spec.server_error() {
                if fed >= 1 {
                    assert_eq!(e, want, "handler error must surface verbatim");
                    assert_eq!(out.len(), 1, "exactly one error frame, no more");
                    let v = parse_json(&out[0], "error frame");
                    assert_eq!(
                        v.get("__error__").and_then(|x| x.as_str()),
                        Some(want.as_str()),
                        "error frame must carry the error verbatim: {v}"
                    );
                    return;
                }
            }
            // Truncated feed: the disconnect must surface as an error and the
            // server must have answered only the steps whose input arrived.
            assert_eq!(e, "no data", "expected disconnect error, got: {e}");
            assert_eq!(out.len(), spec.fixture.server_steps_after(fed));
            let expected = spec.server_frames();
            for (i, frame) in out.iter().enumerate() {
                assert_frame_eq(frame, &expected[i], &format!("response frame {i}"));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// T2 — client side: legitimate server responses, failures, disconnects
// ---------------------------------------------------------------------------

#[derive(Arbitrary, Debug, Clone)]
pub struct ClientCase {
    pub fixture: FixtureId,
    pub args_raw: String,
    pub salt: u8,
    pub pre_seed: u8,
    /// 0 = server sends no responses; capped at the full set.
    pub feed: u8,
    /// 255 = none; else the server's k-th response is a failure frame
    /// (what `run_server` emits when a handler step errors).
    pub err_at: u8,
}

/// Number of client send steps completed before the k-th recv (0-based).
fn sends_before_recv(layout: &[Io], k: usize) -> usize {
    let mut recvs = 0;
    let mut sends = 0;
    for io in layout {
        match io {
            Io::S => sends += 1,
            Io::R => {
                if recvs == k {
                    return sends;
                }
                recvs += 1;
            }
        }
    }
    sends
}

/// Drive the client side of one conversation and check the contract.
pub fn run_client_case(c: &ClientCase) {
    let spec = CaseSpec {
        fixture: c.fixture,
        args_raw: c.args_raw.clone(),
        salt: c.salt,
        pre_seed: c.pre_seed,
        fail: false,
    };
    let args = spec.args();

    let responses = spec.server_frames();
    let n = responses.len();
    let fed = (c.feed as usize).min(n);
    let err_at = (c.err_at != 255).then(|| c.err_at as usize);
    let err_at = err_at.filter(|k| *k < fed);

    let mut console = BufferConsole::new();
    let mut input = NoInput;

    if let Some(pe) = spec.parse_error() {
        let mut ep = endpoint_with(vec![b"never read".to_vec()]);
        let result =
            spec.fixture.protocol().run_client(&args, &mut ep, &mut console, &mut input);
        assert_eq!(result.unwrap_err(), pe, "parse error must surface verbatim");
        assert!(outgoing_frames(&ep).is_empty(), "client must not send before parse succeeds");
        assert!(console.lines.is_empty(), "client must not render on parse error");
        assert_eq!(ep.incoming.lock().unwrap().len(), 1, "nothing must be consumed");
        return;
    }

    let mut incoming: Vec<Vec<u8>> = responses[..fed].to_vec();
    if let Some(k) = err_at {
        let msg = format!("srvfail-{k}");
        incoming[k] = ser(&serde_json::json!({ "__error__": msg }));
    }

    let mut ep = endpoint_with(incoming);
    let result = spec.fixture.protocol().run_client(&args, &mut ep, &mut console, &mut input);
    let out = outgoing_frames(&ep);
    let frames = spec.client_frames();
    let layout = spec.fixture.io_layout();

    match err_at {
        Some(k) => {
            assert_eq!(
                result.unwrap_err(),
                format!("srvfail-{k}"),
                "server failure must surface verbatim to the client"
            );
            assert_eq!(
                console.lines,
                spec.fixture.expected_render_prefix(&spec, k),
                "renders from before the failure must be kept, none after"
            );
            let sent = sends_before_recv(layout, k);
            assert_eq!(out.len(), sent, "client frames sent before the failure");
            for (i, frame) in out.iter().enumerate() {
                assert_frame_eq(frame, &frames[i], &format!("client frame {i}"));
            }
        }
        None if fed < n => {
            assert_eq!(
                result.unwrap_err(),
                "no data",
                "server disconnect must surface as an error"
            );
            assert_eq!(
                console.lines,
                spec.fixture.expected_render_prefix(&spec, fed),
                "renders from received responses must be kept"
            );
            let sent = sends_before_recv(layout, fed);
            assert_eq!(out.len(), sent, "client frames sent before the disconnect");
            for (i, frame) in out.iter().enumerate() {
                assert_frame_eq(frame, &frames[i], &format!("client frame {i}"));
            }
        }
        None => {
            let raw = result.expect("full conversation must succeed");
            assert_frame_eq(&raw, responses.last().unwrap(), "last server response");
            assert_eq!(console.lines, spec.expected_render(), "rendered output");
            assert_eq!(out.len(), frames.len(), "all client frames sent");
            for (i, frame) in out.iter().enumerate() {
                assert_frame_eq(frame, &frames[i], &format!("client frame {i}"));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// T3 — round trip: real socketpair, real dispatch, both sides in-process
// ---------------------------------------------------------------------------

#[derive(Arbitrary, Debug, Clone)]
pub struct RoundTripCase {
    pub fixture: FixtureId,
    pub args_raw: String,
    pub salt: u8,
    pub pre_seed: u8,
    pub fail: bool,
}

/// Run one full client↔server conversation over a real socket pair and
/// check that both sides terminate and agree on the outcome.
pub fn run_round_trip_case(c: &RoundTripCase) {
    use std::io::BufReader;
    use std::os::unix::net::UnixStream;

    use servyi_servatui::connection::TypedConnection;

    let spec = CaseSpec {
        fixture: c.fixture,
        args_raw: c.args_raw.clone(),
        salt: c.salt,
        pre_seed: c.pre_seed,
        fail: c.fail,
    };
    if spec.parse_error().is_some() {
        return; // client bails before any wire traffic; nothing to round-trip
    }

    let (client_sock, server_sock) = UnixStream::pair().expect("socketpair");
    let server_reader = BufReader::new(server_sock.try_clone().expect("clone server stream"));
    let server_conn = SocketConnection { stream: server_sock, reader: server_reader };
    let client_reader = BufReader::new(client_sock.try_clone().expect("clone client stream"));
    let client_conn = SocketConnection { stream: client_sock, reader: client_reader };

    let protocols = FixtureId::all_protocols();
    let ctx = std::sync::Arc::new(CtxState::new(c.pre_seed as usize));
    let ctx_for_thread = std::sync::Arc::clone(&ctx);
    let cmd: String = spec.fixture.name().to_string();

    let server = std::thread::spawn(move || {
        let mut conn = server_conn;
        ServerHandle::handle_connection(&protocols, &mut conn, &*ctx_for_thread)
    });

    let mut conn = client_conn;
    conn.send_typed(&cmd).expect("send command name");
    let mut console = BufferConsole::new();
    let client_result =
        spec.fixture.protocol().run_client(&spec.args(), &mut conn, &mut console, &mut NoInput);
    drop(conn); // never leave the server waiting for a client that is done

    let server_result = server.join().expect("server thread must not panic");

    match spec.server_error() {
        Some(want) => {
            assert_eq!(client_result.unwrap_err(), want, "client must see the handler error");
            assert_eq!(server_result.unwrap_err(), want, "server must return the handler error");
        }
        None => {
            client_result.expect("client must succeed");
            server_result.expect("server must succeed");
            assert_eq!(console.lines, spec.expected_render(), "rendered output");
            if spec.fixture == FixtureId::Ctx {
                assert_eq!(ctx.len(), c.pre_seed as usize + 1, "ctx step must append exactly one entry");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// T4 — TUI mouse selection: realistic user event sequences
// ---------------------------------------------------------------------------

#[derive(Arbitrary, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FuzzButton {
    Left,
    Right,
    Middle,
}

#[derive(Arbitrary, Debug, Clone)]
pub enum FuzzEv {
    Down { button: FuzzButton, col: u8, row: u8, ctrl: bool },
    Up { button: FuzzButton, col: u8, row: u8 },
    Drag { button: FuzzButton, col: u8, row: u8, ctrl: bool },
    ScrollUp { col: u8, row: u8 },
    ScrollDown { col: u8, row: u8 },
    Moved { col: u8, row: u8 },
    /// New output arrives while the user is interacting (realistic mid-drag).
    AppendLine { salt: u8 },
    /// Terminal resize (SIGWINCH), also realistic mid-drag.
    Resize { w: u8, h: u8 },
}

#[derive(Arbitrary, Debug, Clone)]
pub struct MouseCase {
    pub lines: Vec<String>,
    pub term_w: u8,
    pub term_h: u8,
    pub scroll0: u8,
    pub events: Vec<FuzzEv>,
}

/// Rebuild the viewport cache the way a draw pass would: clamp scroll to the
/// content height and recompute the viewport window (mirrors `tui_loop`).
fn rebuild_viewport(state: &mut servyi_servatui::TuiState, w: u16, h: u16) -> servyi_servatui::tui::ViewportCache {
    let log_area = ratatui::layout::Rect::new(0, 0, w, h.saturating_sub(3).max(4));
    let log_inner = log_area.inner(ratatui::layout::Margin { horizontal: 1, vertical: 1 });
    let max_scroll = state.log_lines.len().saturating_sub(log_inner.height as usize);
    state.scroll_up = (state.scroll_up as usize).min(max_scroll) as u16;
    let viewport_start = max_scroll.saturating_sub(state.scroll_up as usize);
    servyi_servatui::tui::ViewportCache {
        log_area,
        log_inner,
        viewport_start,
        total_wrapped: state.log_lines.len(),
        wrapped: state.log_lines.clone(),
        // Harness lines are unwrapped (each row is its own original line),
        // so every row's offset into its original line is 0.
        offsets: vec![0; state.log_lines.len()],
    }
}

fn crossterm_event(
    ev: &FuzzEv,
    w: u16,
    h: u16,
) -> crossterm::event::MouseEvent {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    let on_screen = |col: u8, row: u8| (col as u16 % w, row as u16 % h);
    match ev {
        FuzzEv::Down { button, col, row, ctrl } => {
            let (column, row) = on_screen(*col, *row);
            MouseEvent {
                kind: MouseEventKind::Down(match button {
                    FuzzButton::Left => MouseButton::Left,
                    FuzzButton::Right => MouseButton::Right,
                    FuzzButton::Middle => MouseButton::Middle,
                }),
                column,
                row,
                modifiers: if *ctrl { KeyModifiers::CONTROL } else { KeyModifiers::NONE },
            }
        }
        FuzzEv::Up { button, col, row } => {
            let (column, row) = on_screen(*col, *row);
            MouseEvent {
                kind: MouseEventKind::Up(match button {
                    FuzzButton::Left => MouseButton::Left,
                    FuzzButton::Right => MouseButton::Right,
                    FuzzButton::Middle => MouseButton::Middle,
                }),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            }
        }
        FuzzEv::Drag { button, col, row, ctrl } => {
            let (column, row) = on_screen(*col, *row);
            MouseEvent {
                kind: MouseEventKind::Drag(match button {
                    FuzzButton::Left => MouseButton::Left,
                    FuzzButton::Right => MouseButton::Right,
                    FuzzButton::Middle => MouseButton::Middle,
                }),
                column,
                row,
                modifiers: if *ctrl { KeyModifiers::CONTROL } else { KeyModifiers::NONE },
            }
        }
        FuzzEv::ScrollUp { col, row } => {
            let (column, row) = on_screen(*col, *row);
            MouseEvent { kind: MouseEventKind::ScrollUp, column, row, modifiers: KeyModifiers::NONE }
        }
        FuzzEv::ScrollDown { col, row } => {
            let (column, row) = on_screen(*col, *row);
            MouseEvent { kind: MouseEventKind::ScrollDown, column, row, modifiers: KeyModifiers::NONE }
        }
        FuzzEv::Moved { col, row } => {
            let (column, row) = on_screen(*col, *row);
            MouseEvent { kind: MouseEventKind::Moved, column, row, modifiers: KeyModifiers::NONE }
        }
        FuzzEv::AppendLine { .. } | FuzzEv::Resize { .. } => {
            unreachable!("non-mouse events are handled by the runner")
        }
    }
}

/// Drive the TUI mouse state machine through a realistic event sequence and
/// check behavioral invariants after every event.
pub fn run_mouse_case(c: &MouseCase) {
    use crossterm::event::MouseButton;
    use servyi_servatui::tui as tui;

    let mut w = 20 + c.term_w as u16 % 100; // 20..=119
    let mut h = 8 + c.term_h as u16 % 42; // 8..=49

    let mut state = servyi_servatui::TuiState::new();
    state.log_lines = c
        .lines
        .iter()
        .enumerate()
        .map(|(i, l)| sprout(l, i as u8))
        .collect();
    if state.log_lines.is_empty() {
        state.log_lines.push("seed line".into());
    }
    state.scroll_up = c.scroll0 as u16;

    let mut vp = rebuild_viewport(&mut state, w, h);
    let mut mouse = tui::MouseState::default();

    for (i, ev) in c.events.iter().enumerate() {
        match ev {
            FuzzEv::AppendLine { salt } => {
                // Mirrors tui_loop's Enter handling: append, reset scroll,
                // clear selection (new output).
                state.log_lines.push(format!("new {salt}"));
                state.scroll_up = 0;
                state.selection = None;
                vp = rebuild_viewport(&mut state, w, h);
            }
            FuzzEv::Resize { w: nw, h: nh } => {
                w = 20 + *nw as u16 % 100;
                h = 8 + *nh as u16 % 42;
                vp = rebuild_viewport(&mut state, w, h);
            }
            _ => {
                let m = crossterm_event(ev, w, h);
                let inner = vp.log_inner;
                let area = vp.log_area;
                let scrollbar_col = area.x + area.width - 1;
                let on_scrollbar = m.column == scrollbar_col
                    && m.row > area.y
                    && m.row < area.y + area.height - 1;
                let in_log = m.column >= inner.x
                    && m.column < inner.x + inner.width
                    && m.row >= inner.y
                    && m.row < inner.y + inner.height;

                let expected_scrollbar_press = if let crossterm::event::MouseEventKind::Down(MouseButton::Left) = m.kind {
                    on_scrollbar.then(|| {
                        let track_h = (area.height.saturating_sub(2)) as usize;
                        let click_y = (m.row - area.y - 1) as usize;
                        let max_scroll = vp.total_wrapped.saturating_sub(inner.height as usize);
                        max_scroll.saturating_sub(click_y * max_scroll / track_h.max(1)) as u16
                    })
                } else {
                    None
                };

                let selection_before = state.selection;
                tui::handle_mouse_event(m, &mut state, &mut mouse, &vp);

                if let Some(expected) = expected_scrollbar_press {
                    assert_eq!(
                        state.scroll_up, expected,
                        "scrollbar press must jump to the clicked position"
                    );
                }
                if let crossterm::event::MouseEventKind::Down(MouseButton::Left) = m.kind {
                    if in_log && !on_scrollbar {
                        let crow = vp.viewport_start + (m.row - inner.y) as usize;
                        // All click modes anchor the selection on the
                        // clicked row — except a word-mode click below the
                        // content, which leaves the previous selection.
                        if crow < vp.wrapped.len() {
                            let (sr, _, er, _) = state
                                .selection
                                .unwrap_or_else(|| panic!("event {i}: click on content must open a selection"));
                            assert_eq!(
                                (sr, er), (crow, crow),
                                "event {i}: click must anchor the selection on the clicked row"
                            );
                        }
                    }
                }
                // NOTE: we deliberately do NOT assert that a drag with
                // `selection_before == None` keeps the selection empty: a
                // right-click outside the log clears the visible selection
                // while the (still held) left-drag anchor persists, so a
                // subsequent drag legitimately re-opens the selection.

                if let Some((sr, sc, er, ec)) = state.selection {
                    let text = tui::extract_selection(&vp.wrapped, &vp.offsets, sr, sc, er, ec, state.selection_rect);
                    let total: usize = vp.wrapped.iter().map(|l| l.chars().count()).sum();
                    assert!(
                        text.chars().count() <= total + vp.wrapped.len(),
                        "event {i}: extracted text larger than the whole content"
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// T5 — log rendering: adversarial selections and areas into a real buffer
// ---------------------------------------------------------------------------

#[derive(Arbitrary, Debug, Clone)]
pub struct RenderCase {
    pub lines: Vec<String>,
    pub has_sel: bool,
    pub sr: u8,
    pub sc: u8,
    pub er: u8,
    pub ec: u8,
    pub viewport_start: u8,
    pub rect: bool,
    pub area_x: u8,
    pub area_y: u8,
    pub area_w: u8,
    pub area_h: u8,
}

/// Render pre-wrapped lines with an arbitrary (possibly stale or out-of-
/// range) selection into a real ratatui buffer. Must never panic.
pub fn run_render_case(c: &RenderCase) {
    use ratatui::widgets::WidgetRef;

    let lines: Vec<String> = c.lines.iter().enumerate().map(|(i, l)| sprout(l, i as u8)).collect();
    let selection = c.has_sel.then(|| {
        (c.sr as usize, c.sc as usize, c.er as usize, c.ec as usize)
    });
    let widget = servyi_servatui::tui::LogWidget {
        lines: lines.clone(),
        selection,
        viewport_start: c.viewport_start as usize,
        rect: c.rect,
        offsets: vec![0; lines.len()],
    };
    let area = ratatui::layout::Rect::new(
        c.area_x as u16 % 80,
        c.area_y as u16 % 40,
        c.area_w as u16 % 80,
        c.area_h as u16 % 40,
    );
    let mut buf = ratatui::buffer::Buffer::empty(area);
    widget.render_ref(area, &mut buf);
}
