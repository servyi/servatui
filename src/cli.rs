//! clap patterns for protocol commands.
//!
//! A protocol command carries its own clap pattern — plain
//! `clap::Arg`s, the vocabulary clap already defines. Clients combine
//! a complete CLI with [`clap_tree`] (their top-level command plus
//! one subcommand per protocol), and TUIs can read the same shapes
//! (e.g. to render a `help` command). Nothing framework-specific is
//! invented: the args are clap's own (typed value parsers included),
//! their DECLARATION order is the wire args-string order (flags
//! included), and [`args_string`] serializes matched values back in
//! that order via the raw (already validated) values.

/// The clap subcommand for one protocol command: its name, its help,
/// and its declared args.
pub fn subcommand(p: &crate::Protocol) -> clap::Command {
    let mut sub = clap::Command::new(p.name).about(p.help);
    for a in &p.clap_args {
        sub = sub.arg(a.clone());
    }
    sub
}

/// Combine a COMPLETE clap tree: `top` is the caller's command —
/// binary name, about, top-level options (`--socket`, ...), and any
/// local subcommands (`restart`, ...) — and every protocol becomes a
/// subcommand carrying its own args.
pub fn clap_tree(mut top: clap::Command, protocols: &[crate::Protocol]) -> clap::Command {
    for p in protocols {
        top = top.subcommand(subcommand(p));
    }
    top
}

/// Serialize matched values into the wire ARGS-STRING, in the
/// pattern's declaration order. Optional args that were not supplied
/// simply drop out. Values are the RAW argv values — clap's value
/// parsers have already validated them at parse time.
/// Separator of the wire ARGS-STRING fields.
const ARGS_FIELD_SEPARATOR: &str = " ";

pub fn args_string(args: &[clap::Arg], matches: &clap::ArgMatches) -> String {
    let mut parts: Vec<String> = Vec::new();
    for a in args {
        if let Some(v) = matches.get_raw(a.get_id().as_str()).and_then(|mut raw| raw.next()) {
            parts.push(v.to_string_lossy().into_owned());
        }
    }
    parts.join(ARGS_FIELD_SEPARATOR)
}

#[cfg(test)]
#[allow(unknown_lints)] // plain clippy does not know the driver's lints
#[allow(inline_literal)] // scenario content: the literals ARE the test data
mod tests {
    use super::{args_string, clap_tree};

    /// Scenario vocabulary of the CLI tests: arg ids, long flags,
    /// subcommand names, and the raw argv samples they parse.
    const ID_NAME: &str = "name";
    const ID_FILE: &str = "file";
    const ID_HASH: &str = "hash";
    const ID_WHY: &str = "why";
    const ID_ID: &str = "id";
    const ID_SOCKET: &str = "socket";
    fn flag(id: &str) -> String {
        std::format!("--{id}")
    }
    const SUB_ADD: &str = "add";
    const ABOUT_ADD: &str = "add a secret";
    const SUB_GRANT: &str = "grant";
    const ABOUT_GRANT: &str = "grant one";
    const SUB_STATUS: &str = "status";
    const ABOUT_STATUS: &str = "show everything";
    const SUB_RESTART: &str = "restart";
    const ABOUT_RESTART: &str = "local restart";
    const TOOL_NAME: &str = "tool";
    const SAMPLE_HASH: &str = "h1";
    const SAMPLE_NAME: &str = "n1";
    const SAMPLE_PATH: &str = "/tmp/f";
    const SAMPLE_ID: &str = "7";
    const SAMPLE_GARBAGE_ID: &str = "seven";
    const SAMPLE_SOCKET: &str = "/s";
    use clap::Arg;

    /// The pattern is clap's own vocabulary: declaration order is the
    /// args-string order (flags included) however the user orders them.
    #[test]
    fn serializes_in_declaration_order_regardless_of_user_order() {
        let args = vec![
            Arg::new(ID_NAME).required(true),
            Arg::new(ID_FILE)
                .long(ID_FILE)
                .required(true)
                .value_parser(clap::value_parser!(std::path::PathBuf)),
            Arg::new(ID_HASH).long(ID_HASH).required(true),
        ];
        let hash_flag = flag(ID_HASH);
        let file_flag = flag(ID_FILE);
        let m = subcommand_args(SUB_ADD, ABOUT_ADD, &args)
            .try_get_matches_from([
                SUB_ADD,
                hash_flag.as_str(),
                SAMPLE_HASH,
                SAMPLE_NAME,
                file_flag.as_str(),
                SAMPLE_PATH,
            ])
            .expect("matches");
        assert_eq!(args_string(&args, &m), "n1 /tmp/f h1");
    }

    #[test]
    fn typed_parsers_reject_garbage_and_missing_optionals_drop_out() {
        let args = vec![
            Arg::new(ID_ID).required(true).value_parser(clap::value_parser!(u64)),
            Arg::new(ID_WHY).long(ID_WHY).required(false),
        ];
        let cmd = subcommand_args(SUB_GRANT, ABOUT_GRANT, &args);
        let m = cmd.try_get_matches_from([SUB_GRANT, SAMPLE_ID]).expect("matches");
        assert_eq!(args_string(&args, &m), "7");
        assert!(
            subcommand_args(SUB_GRANT, ABOUT_GRANT, &args)
                .try_get_matches_from([SUB_GRANT, SAMPLE_GARBAGE_ID])
                .is_err(),
            "u64 parser must reject garbage"
        );
        assert!(
            subcommand_args(SUB_GRANT, ABOUT_GRANT, &args)
                .try_get_matches_from([SUB_GRANT])
                .is_err(),
            "required arg enforced"
        );
    }

    /// clap_tree folds the caller's extras and every protocol into one
    /// complete tree, and the whole thing parses.
    #[test]
    fn clap_tree_combines_extras_and_protocols() {
        use crate::ShellAction;
        let protocols = vec![
            crate::Plugin::new(SUB_STATUS, ABOUT_STATUS)
                .parse(|_: &str| Ok(()))
                .client(|v: (), _out, _input| Ok(v))
                .server(|v: ()| -> Result<(), String> { Ok(v) })
                .finalize(|| Ok(ShellAction::Continue)),
            crate::Plugin::new(SUB_GRANT, ABOUT_GRANT)
                .parse(|s: &str| s.trim().parse::<u64>().map_err(|e| e.to_string()))
                .client(|v: u64, _out, _input| Ok(v))
                .server(|v: u64| -> Result<u64, String> { Ok(v) })
                .finalize(|| Ok(ShellAction::Continue))
                .clap_arg(Arg::new(ID_ID).required(true).value_parser(clap::value_parser!(u64))),
        ];
        let top = clap::Command::new(TOOL_NAME)
            .arg(Arg::new(ID_SOCKET).long(ID_SOCKET).global(true))
            .subcommand(clap::Command::new(SUB_RESTART).about(ABOUT_RESTART));
        let make_tree = || clap_tree(top.clone(), &protocols);
        make_tree().debug_assert();
        let m = make_tree()
        .try_get_matches_from([
            TOOL_NAME,
            flag(ID_SOCKET).as_str(),
            SAMPLE_SOCKET,
            SUB_GRANT,
            SAMPLE_ID,
        ])
            .expect("whole tree parses");
        assert_eq!(m.get_one::<String>("socket").map(String::as_str), Some("/s"));
        let (name, sub) = m.subcommand().expect("grant matched");
        assert_eq!(name, "grant");
        assert_eq!(args_string(&protocols[1].clap_args, sub), SAMPLE_ID);
        // extras survive
        let whole = make_tree();
        let names: Vec<&str> = whole.get_subcommands().map(|c| c.get_name()).collect();
        assert!(names.contains(&"restart") && names.contains(&"status"));
    }

    fn subcommand_args(name: &'static str, help: &'static str, args: &[Arg]) -> clap::Command {
        let mut sub = clap::Command::new(name).about(help);
        for a in args {
            sub = sub.arg(a.clone());
        }
        sub
    }
}
