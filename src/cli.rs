//! clap patterns for protocol commands (behind the `cli` feature).
//!
//! A protocol command can carry its own clap pattern — plain
//! [`clap::Arg`]s, the vocabulary clap already defines — so a client
//! COMBINES its full clap tree from the registered protocols plus
//! whatever local commands and top-level options it adds. Nothing
//! framework-specific is invented: the args are clap's own (typed
//! value parsers included), their DECLARATION order is the wire
//! args-string order (flags included), and [`args_string`]
//! serializes matched values back in that order via the raw (already
//! validated) values.

/// The clap subcommand for one protocol command: its name, its help,
/// and its declared args.
#[cfg(feature = "cli")]
pub fn subcommand(p: &crate::Protocol) -> clap::Command {
    let mut sub = clap::Command::new(p.name).about(p.help);
    for a in &p.clap_args {
        sub = sub.arg(a.clone());
    }
    sub
}

/// Serialize matched values into the wire ARGS-STRING, in the
/// pattern's declaration order. Optional args that were not supplied
/// simply drop out. Values are the RAW argv values — clap's value
/// parsers have already validated them at parse time.
#[cfg(feature = "cli")]
pub fn args_string(args: &[clap::Arg], matches: &clap::ArgMatches) -> String {
    let mut parts: Vec<String> = Vec::new();
    for a in args {
        if let Some(v) = matches
            .get_raw(a.get_id().as_str())
            .and_then(|mut raw| raw.next())
        {
            parts.push(v.to_string_lossy().into_owned());
        }
    }
    parts.join(" ")
}

#[cfg(all(feature = "cli", test))]
mod tests {
    use super::args_string;
    use clap::Arg;

    /// The pattern is clap's own vocabulary: declaration order is the
    /// args-string order (flags included) however the user orders them.
    #[test]
    fn serializes_in_declaration_order_regardless_of_user_order() {
        let args = vec![
            Arg::new("name").required(true),
            Arg::new("file").long("file").required(true).value_parser(clap::value_parser!(std::path::PathBuf)),
            Arg::new("hash").long("hash").required(true),
        ];
        let m = subcommand_args("add", "add a secret", &args)
            .try_get_matches_from(["add", "--hash", "h1", "n1", "--file", "/tmp/f"])
            .expect("matches");
        assert_eq!(args_string(&args, &m), "n1 /tmp/f h1");
    }

    #[test]
    fn typed_parsers_reject_garbage_and_missing_optionals_drop_out() {
        let args = vec![
            Arg::new("id").required(true).value_parser(clap::value_parser!(u64)),
            Arg::new("why").long("why").required(false),
        ];
        let cmd = subcommand_args("grant", "grant one", &args);
        let m = cmd
            .try_get_matches_from(["grant", "7"])
            .expect("matches");
        assert_eq!(args_string(&args, &m), "7");
        assert!(
            subcommand_args("grant", "g", &args)
                .try_get_matches_from(["grant", "seven"])
                .is_err(),
            "u64 parser must reject garbage"
        );
        assert!(
            subcommand_args("grant", "g", &args)
                .try_get_matches_from(["grant"])
                .is_err(),
            "required arg enforced"
        );
    }

    fn subcommand_args(name: &'static str, help: &'static str, args: &[Arg]) -> clap::Command {
        let mut sub = clap::Command::new(name).about(help);
        for a in args {
            sub = sub.arg(a.clone());
        }
        sub
    }
}
