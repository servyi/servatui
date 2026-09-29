//! Argument-shape vocabulary for deriving surfaces from one command
//! table (issue #5).
//!
//! The ONE table is the consumer's command spec (name, help, parse,
//! completer, offline) — servatui's [`crate::protocol`] builders
//! consume it directly. What that table cannot express, and what
//! derived surfaces (CLI argument trees, validation) need, is the
//! SHAPE of each command's positional arguments: this vocabulary.
//!
//! A consumer's spec gains `args: &[ArgSpec]` and every surface reads
//! the same row: the wire args-string parser documents/validates
//! against it, the CLI tree derives from it. Completion is not
//! described here — the completer closure registered on the protocol
//! IS the completion surface (TUI tab-complete, `--complete` queries,
//! and the `servatui-grammar` walker all consume that closure
//! verbatim).

/// The shape of one positional argument of a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgKind {
    /// A free-form string (a name, a hash).
    Str,
    /// A decimal number (ids).
    U64,
    /// A filesystem path (a file to read, a socket).
    Path,
}

/// One positional argument of a command's args-string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgSpec {
    pub name: &'static str,
    pub kind: ArgKind,
    /// Optional arguments may be omitted from the args-string;
    /// arguments after the first optional one must themselves be
    /// optional.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arg_specs_are_const_constructible() {
        const ARGS: &[ArgSpec] = &[
            ArgSpec::required("id", ArgKind::U64),
            ArgSpec::optional("reason", ArgKind::Str),
            ArgSpec::required("file", ArgKind::Path),
        ];
        assert_eq!(ARGS[0].name, "id");
        assert!(!ARGS[0].optional);
        assert_eq!(ARGS[0].kind, ArgKind::U64);
        assert!(ARGS[1].optional);
        assert_eq!(ARGS[2].kind, ArgKind::Path);
        assert_eq!(ARGS.len(), 3);
    }
}
