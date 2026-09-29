//! Declarative command metadata: the single source of truth from
//! which every surface derives (issue #5) — the protocol registration
//! itself, tab completion, and CLI argument trees.
//!
//! The framework stays generic: [`ArgSpec`] describes shape, and
//! [`CompletionSource`] describes WHERE completion options come from
//! (statically listed, a named live snapshot, or nowhere).  Closures
//! and CLI trees can be derived from both; nothing requires executing
//! the command to learn its shape.  Fuzzing is deliberately NOT here:
//! a grammar over commands is project-specific and may be infinite —
//! a lazy random walk belongs in a consumer-side crate, not the
//! framework (see the PR discussion).

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
/// tab completion and CLI derivation both read the
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
    /// KIND of snapshot so generators (fuzzing, completion queries) can supply
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
/// registration.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completer_completes_last_word_full_line() {
        let src = CompletionSource::Static(&["alpha", "beta"]);
        let comp = src.completer(|_| Vec::new()).unwrap();
        let got = comp("mode al");
        assert_eq!(got, vec!["mode alpha".to_string()]);
    }
}
