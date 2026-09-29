//! Declarative command metadata: the single source of truth from
//! which every surface derives (issue #5) — the protocol registration
//! itself, tab completion, CLI argument trees, and fuzz grammars.
//!
//! The framework stays generic: [`ArgSpec`] describes shape, and
//! [`CompletionSource`] describes WHERE completion options come from.
//! Closures can be derived from both; fuzz grammars and CLI builders
//! can be derived from both; nothing requires executing the command to
//! learn its grammar.

use std::sync::Arc;

/// The shape of one positional argument of a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgKind {
    /// A free-form string (a name, a hash, a path).
    Str,
    /// A decimal number (ids).
    U64,
    /// A filesystem path (a file to read, a socket).
    Path,
}

/// One positional argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgSpec {
    pub name: &'static str,
    pub kind: ArgKind,
    /// Arguments after the first optional one must themselves be
    /// optional; `Option<Option<T>>` shapes are expressed by arity,
    /// not nesting.
    pub optional: bool,
}

impl ArgSpec {
    pub const fn required(name: &'static str, kind: ArgKind) -> Self {
        Self { name, kind, optional: false }
    }
    pub const fn optional(name: &'static str, kind: ArgKind) -> Self {
        Self { name, kind, optional: true }
    }
}

/// Where a completion's options come from — DATA, not a closure, so
/// tab completion, CLI derivation, and fuzz grammars all read the
/// same source without executing anything (issue #5 point 2/3).
#[derive(Clone)]
pub enum CompletionSource {
    /// No argument completion (the command's own name is the only
    /// completion, handled by the shell's command list).
    None,
    /// A fixed set of options known at build time.
    Static(&'static [&'static str]),
    /// Options from a live snapshot the client's poller refreshes —
    /// e.g. pending-request ids, served names. The string names the
    /// KIND of snapshot so a grammar generator (fuzzing) can supply
    /// its own values of the right shape.
    Snapshot(&'static str),
}

impl CompletionSource {
    /// The completer closure this source implies, with the snapshot
    /// values supplied by `snapshot: Fn(kind) -> Vec<String>`.
    pub fn completer<F>(&self, snapshot: F) -> Option<CompleterFn>
    where
        F: Fn(&str) -> Vec<String> + Send + Sync + 'static,
    {
        match self {
            CompletionSource::None => None,
            CompletionSource::Static(options) => {
                let options: Vec<String> = options.iter().map(|s| s.to_string()).collect();
                Some(Arc::new(move |confirmed: &str| {
                    // Complete the LAST word against the options
                    // (full-line suggestions, the Completer contract).
                    let prefix = confirmed
                        .rsplit_once(char::is_whitespace)
                        .map(|(_, last)| last)
                        .unwrap_or("");
                    options
                        .iter()
                        .filter(|o| o.starts_with(prefix))
                        .map(|o| {
                            let head = confirmed
                                .rsplit_once(char::is_whitespace)
                                .map(|(head, _)| format!("{head} "))
                                .unwrap_or_default();
                            format!("{head}{o}")
                        })
                        .collect()
                }))
            }
            CompletionSource::Snapshot(kind) => {
                let kind = kind.to_string();
                Some(Arc::new(move |confirmed: &str| {
                    let options = snapshot(&kind);
                    let prefix = confirmed
                        .rsplit_once(char::is_whitespace)
                        .map(|(_, last)| last)
                        .unwrap_or("");
                    options
                        .into_iter()
                        .filter(|o| o.starts_with(prefix))
                        .map(|o| {
                            let head = confirmed
                                .rsplit_once(char::is_whitespace)
                                .map(|(head, _)| format!("{head} "))
                                .unwrap_or_default();
                            format!("{head}{o}")
                        })
                        .collect()
                }))
            }
        }
    }
}

/// The completer closure type (re-exported shape of
/// [`crate::protocol::Completer`], aliasable for builders).
pub type CompleterFn = Arc<dyn Fn(&str) -> Vec<String> + Send + Sync>;

/// The full declarative description of one command — the single
/// source of truth. A downstream crate declares a table of these and
/// derives: the `Protocol` (parse + steps stay hand-written where
/// they carry logic), the CLI argument tree, the completion
/// registration, and the fuzz grammar.
#[derive(Clone)]
pub struct CommandDef {
    pub name: &'static str,
    pub help: &'static str,
    /// Positional arguments in order.
    pub args: &'static [ArgSpec],
    /// Where argument completion options come from.
    pub completion: CompletionSource,
}

impl CommandDef {
    pub const fn new(name: &'static str, help: &'static str) -> Self {
        Self { name, help, args: &[], completion: CompletionSource::None }
    }

    pub const fn args(mut self, args: &'static [ArgSpec]) -> Self {
        self.args = args;
        self
    }

    pub const fn completion(mut self, source: CompletionSource) -> Self {
        self.completion = source;
        self
    }
}

/// A fuzz grammar over a command table (issue #5 point 3): every
/// command appears with nonzero probability, and every completion
/// option contributes candidates — `Static` enumerates directly,
/// `Snapshot` kinds get caller-supplied values of the right shape.
pub struct Grammar<'a> {
    defs: &'a [CommandDef],
    snapshot_values: Vec<(&'static str, Vec<&'static str>)>,
}

impl<'a> Grammar<'a> {
    pub fn new(defs: &'a [CommandDef]) -> Self {
        Self { defs, snapshot_values: Vec::new() }
    }

    /// Supply the candidate values for one snapshot kind (e.g. two
    /// plausible pending ids). Unsupplied kinds fall back to one
    /// synthesized value so no command drops out of the grammar —
    /// every command keeps a nonzero probability, per the issue.
    pub fn with_snapshot(mut self, kind: &'static str, values: &[&'static str]) -> Self {
        self.snapshot_values.push((kind, values.to_vec()));
        self
    }

    fn snapshot_for(&self, kind: &str) -> Vec<String> {
        for (k, vs) in &self.snapshot_values {
            if *k == kind {
                return vs.iter().map(|s| s.to_string()).collect();
            }
        }
        // One synthesized value of a plausible shape per kind.
        vec![match kind {
            k if k.contains("id") || k.contains("Id") => "1".to_string(),
            k if k.contains("name") || k.contains("Name") => "name".to_string(),
            _ => "value".to_string(),
        }]
    }

    /// One weighted draw of a full command line. `rng` picks the
    /// command (every command nonzero), and the option index draws
    /// from the remaining entropy so EVERY completion option is
    /// reachable with nonzero probability — the issue's grammar
    /// requirement.
    pub fn command_line(&self, rng: u64) -> String {
        let total = self.total_weight();
        let mut pick = rng % total;
        // Entropy beyond command selection drives the option index.
        let option_entropy = rng / total;
        for def in self.defs {
            let w = self.weight(def);
            if pick < w {
                return self.line_for(def, option_entropy);
            }
            pick -= w;
        }
        // Unreachable: weights sum to total.
        self.line_for(&self.defs[0], option_entropy)
    }

    /// Nonzero weight per command: arguments multiply the reachable
    /// lines, but every command keeps weight >= 1 — the issue's
    /// "nonzero probabilities to each" guarantee.
    pub fn weight(&self, def: &CommandDef) -> u64 {
        1 + def.args.len() as u64
    }

    pub fn total_weight(&self) -> u64 {
        self.defs.iter().map(|d| self.weight(d)).sum()
    }

    fn line_for(&self, def: &CommandDef, option_entropy: u64) -> String {
        let mut line = def.name.to_string();
        // The options for the completed argument (empty for None).
        let options: Vec<String> = match &def.completion {
            CompletionSource::None => Vec::new(),
            CompletionSource::Static(options) => {
                options.iter().map(|s| s.to_string()).collect()
            }
            CompletionSource::Snapshot(kind) => self.snapshot_for(kind),
        };
        // The first argument takes a drawn option (round-robin by
        // entropy); every option reachable with nonzero probability.
        let mut arg_index = 0usize;
        if !options.is_empty() {
            let idx = (option_entropy % options.len() as u64) as usize;
            line.push(' ');
            line.push_str(&options[idx]);
            arg_index = 1;
        }
        // Remaining required arguments get shape-plausible values so
        // the parse layer sees a well-formed line.
        for arg in def.args.iter().skip(arg_index) {
            if !arg.optional {
                line.push(' ');
                line.push_str(match arg.kind {
                    ArgKind::U64 => "1",
                    ArgKind::Path => "/tmp/x",
                    ArgKind::Str => "x",
                });
            }
        }
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDS: &[&str] = &["7", "42"];
    const DEFS: &[CommandDef] = &[
        CommandDef::new("status", "show everything"),
        CommandDef::new("grant", "grant one")
            .args(&[ArgSpec::required("id", ArgKind::U64)])
            .completion(CompletionSource::Snapshot("pending-ids")),
        CommandDef::new("mode", "set mode")
            .args(&[ArgSpec::required("mode", ArgKind::Str)])
            .completion(CompletionSource::Static(&["on", "off"])),
    ];

    #[test]
    fn every_command_has_nonzero_probability() {
        let g = Grammar::new(DEFS).with_snapshot("pending-ids", IDS);
        let mut seen: Vec<String> = Vec::new();
        for seed in 0..200u64 {
            let line = g.command_line(seed);
            let word = line.split_whitespace().next().unwrap().to_string();
            if !seen.contains(&word) {
                seen.push(word);
            }
        }
        for def in DEFS {
            assert!(seen.iter().any(|s| s == def.name), "{} never drawn", def.name);
        }
    }

    #[test]
    fn static_and_snapshot_options_are_reachable() {
        let g = Grammar::new(DEFS).with_snapshot("pending-ids", IDS);
        let mut saw_on = false;
        let mut saw_off = false;
        let mut saw_42 = false;
        for seed in 0..500u64 {
            let line = g.command_line(seed);
            if line == "mode on" { saw_on = true; }
            if line == "mode off" { saw_off = true; }
            if line == "grant 42" { saw_42 = true; }
        }
        assert!(saw_on && saw_off, "static options must be reachable");
        assert!(saw_42, "snapshot-supplied values must be reachable");
    }

    #[test]
    fn completer_completes_last_word_full_line() {
        let src = CompletionSource::Static(&["alpha", "beta"]);
        let comp = src.completer(|_| Vec::new()).unwrap();
        let got = comp("mode al");
        assert_eq!(got, vec!["mode alpha".to_string()]);
    }
}
