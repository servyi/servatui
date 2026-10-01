//! CLI patterns for protocol commands (behind the `cli` feature).
//!
//! A protocol command can carry its own [`CliArg`] pattern — the
//! declarative shape of its command-line arguments — so a client
//! COMBINES the full clap tree from the registered protocols plus
//! whatever local commands and top-level options it adds. One
//! declaration drives the wire parser's grammar documentation, the
//! tree, and the args-string serialization (values serialize in
//! declaration order however the user orders flags).
//!
//! [`CliArg`]/[`CliKind`] are dependency-free data; only the
//! tree-building helpers need clap, hence the feature gate.

/// The value shape of one CLI argument — drives clap's value parser.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CliKind {
    /// A free-form string (a name, a hash).
    Str,
    /// A decimal number (ids).
    U64,
    /// A filesystem path (a file to read, a socket).
    Path,
}

/// One CLI argument of a protocol command, in ARGS-STRING declaration
/// order: values serialize positionally in this order (flags too —
/// `add NAME FILE HASH` keeps its order whatever the user's flag
/// order was), while `flag`-style args present as `--name` on the
/// command line.
#[derive(Clone, Copy, Debug)]
pub struct CliArg {
    pub name: &'static str,
    pub kind: CliKind,
    pub optional: bool,
    pub flag: bool,
}

impl CliArg {
    /// A required positional argument.
    pub const fn pos(name: &'static str, kind: CliKind) -> Self {
        Self { name, kind, optional: false, flag: false }
    }
    /// An optional positional argument.
    pub const fn opt(name: &'static str, kind: CliKind) -> Self {
        Self { name, kind, optional: true, flag: false }
    }
    /// A required `--name` option.
    pub const fn flag(name: &'static str, kind: CliKind) -> Self {
        Self { name, kind, optional: false, flag: true }
    }
    /// An optional `--name` option.
    pub const fn opt_flag(name: &'static str, kind: CliKind) -> Self {
        Self { name, kind, optional: true, flag: true }
    }
}

#[cfg(feature = "cli")]
mod clap_impls {
    use super::{CliArg, CliKind};

    /// The clap subcommand for one protocol command: name, help, and
    /// the typed argument pattern.
    pub fn subcommand(name: &'static str, help: &'static str, args: &[CliArg]) -> clap::Command {
        let mut sub = clap::Command::new(name).about(help);
        for a in args {
            let arg = clap_arg(a);
            sub = sub.arg(arg);
        }
        sub
    }

    /// Serialize matched values into the wire ARGS-STRING, in the
    /// pattern's declaration order. Optional values simply drop out.
    pub fn args_string(args: &[CliArg], matches: &clap::ArgMatches) -> String {
        let mut parts: Vec<String> = Vec::new();
        for a in args {
            let value = match a.kind {
                CliKind::U64 => matches.get_one::<u64>(a.name).map(|v| v.to_string()),
                CliKind::Path => matches
                    .get_one::<std::path::PathBuf>(a.name)
                    .map(|p| p.display().to_string()),
                CliKind::Str => matches.get_one::<String>(a.name).cloned(),
            };
            if let Some(v) = value {
                parts.push(v);
            }
        }
        parts.join(" ")
    }

    fn clap_arg(a: &CliArg) -> clap::Arg {
        let mut arg = if a.flag {
            clap::Arg::new(a.name).long(a.name)
        } else {
            clap::Arg::new(a.name)
        };
        arg = arg.required(!a.optional);
        match a.kind {
            CliKind::U64 => arg.value_parser(clap::value_parser!(u64)),
            CliKind::Path => arg.value_parser(clap::value_parser!(std::path::PathBuf)),
            CliKind::Str => arg,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::super::{CliArg, CliKind};
        use super::{args_string, subcommand};

        /// The pattern builds a working typed subcommand, and values
        /// serialize in DECLARATION order regardless of user order.
        #[test]
        fn subcommand_parses_and_serializes_in_declaration_order() {
            let pattern = [
                CliArg::pos("name", CliKind::Str),
                CliArg::flag("file", CliKind::Path),
                CliArg::flag("hash", CliKind::Str),
            ];
            let make = || subcommand("add", "add a secret", &pattern);
            // flags given in reverse order serialize in declared order
            let m = make()
                .try_get_matches_from(["add", "--hash", "h1", "n1", "--file", "/tmp/f"])
                .expect("matches");
            assert_eq!(args_string(&pattern, &m), "n1 /tmp/f h1");
        }

        #[test]
        fn u64_args_reject_garbage_and_optional_values_drop_out() {
            let pattern = [
                CliArg::pos("id", CliKind::U64),
                CliArg::opt_flag("why", CliKind::Str),
            ];
            let make = || subcommand("grant", "grant one", &pattern);
            let m = make().try_get_matches_from(["grant", "7"]).expect("matches");
            assert_eq!(args_string(&pattern, &m), "7");
            let err = make().try_get_matches_from(["grant", "seven"]);
            assert!(err.is_err(), "u64 kind must reject non-numbers");
            let missing_required = make().try_get_matches_from(["grant"]);
            assert!(missing_required.is_err(), "required positional enforced");
        }
    }
}

#[cfg(feature = "cli")]
pub use clap_impls::{args_string, subcommand};
