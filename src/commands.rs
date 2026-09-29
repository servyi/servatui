//! Declarative command metadata: the single source of truth from
//! which the protocol registration and the CLI argument trees derive
//! (issue #5).
//!
//! The framework stays generic: [`ArgSpec`] describes argument shape.
//! Completion is deliberately NOT described here — the completer
//! closure registered on the protocol IS the completion surface, and
//! every consumer reads it directly: the TUI tab-completes with it,
//! `--complete` queries it, and the `servatui-grammar` walker walks
//! it verbatim. A typed description of a closure between the two
//! would solve no problem (see the PR #6/#7 discussions).

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

/// The full declarative description of one command — the single
/// source of truth for its NAME, HELP, and argument shape. A
/// downstream crate declares a table of these and derives: the
/// `Protocol` (parse + steps stay hand-written where they carry
/// logic) and the CLI argument tree. Completion is registered as a
/// closure on the protocol, not described here.
#[derive(Debug, Clone, Copy)]
pub struct CommandDef {
    pub name: &'static str,
    pub help: &'static str,
    /// Positional arguments in order.
    pub args: &'static [ArgSpec],
}

impl CommandDef {
    pub const fn new(name: &'static str, help: &'static str) -> Self {
        Self { name, help, args: &[] }
    }

    pub const fn args(mut self, args: &'static [ArgSpec]) -> Self {
        self.args = args;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_defs_are_const_constructible() {
        const DEFS: &[CommandDef] = &[
            CommandDef::new("status", "show everything"),
            CommandDef::new("grant", "grant one")
                .args(&[ArgSpec::required("id", ArgKind::U64)]),
            CommandDef::new("mode", "set mode").args(&[
                ArgSpec::required("mode", ArgKind::Str),
                ArgSpec::optional("file", ArgKind::Path),
            ]),
        ];
        assert_eq!(DEFS[0].name, "status");
        assert!(DEFS[0].args.is_empty());
        assert_eq!(DEFS[1].args[0].name, "id");
        assert!(!DEFS[1].args[0].optional);
        assert_eq!(DEFS[2].args.len(), 2);
        assert!(DEFS[2].args[1].optional);
    }
}
