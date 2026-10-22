use std::env;

use anyhow::Result;
use clap::{CommandFactory, Parser};

use crate::app::App;
use crate::cli::{Cli, Command};
use crate::commands::{dispatch, new};

/// Why a `$SSH_ORIGINAL_COMMAND` was not dispatched. Distinct variants so the
/// caller can map each to the right message + exit code, and so tests can pin
/// which stage rejected the input.
#[derive(Debug)]
pub(crate) enum GatewayReject {
    /// No command at all — a bare `ssh host` login. Not an error; print usage.
    Empty,
    /// Unbalanced quotes: nothing sensible to hand the parser.
    Unshlexable,
    /// Split fine but isn't a valid lilbox invocation (unknown subcommand,
    /// bad flag, `--help`, missing positional, ...).
    Unparseable(clap::Error),
    /// A real, valid subcommand that the gateway does not expose remotely.
    NotAllowed(&'static str),
}

/// Subcommands reachable over the gateway. A strict allowlist by construction:
/// anything not matched here (including any future `Command` variant, and
/// `Gateway` itself) is rejected. Deliberately excludes host-reaching commands
/// (`Cp`, `Exec`, `Run`), image/template management, and `Agent`.
fn allowed(command: &Command) -> bool {
    matches!(
        command,
        Command::New(_)
            | Command::Ssh(_)
            | Command::Ls { .. }
            | Command::Stat { .. }
            | Command::Url { .. }
            | Command::Logs(_)
            | Command::Stop { .. }
            | Command::Start { .. }
            | Command::Restart { .. }
            | Command::Rm { .. }
    )
}

/// Human-readable name for a rejected subcommand, for the error message.
fn command_label(command: &Command) -> &'static str {
    match command {
        Command::New(_) => "new",
        Command::Ls { .. } => "ls",
        Command::Gc => "gc",
        Command::Templates => "templates",
        Command::Template(_) => "template",
        Command::Provision { .. } => "provision",
        Command::Exec(_) => "exec",
        Command::Ssh(_) => "ssh",
        Command::Cp { .. } => "cp",
        Command::Logs(_) => "logs",
        Command::Run(_) => "run",
        Command::Expose { .. } => "expose",
        Command::Unexpose { .. } => "unexpose",
        Command::Join { .. } => "join",
        Command::Leave { .. } => "leave",
        Command::Stop { .. } => "stop",
        Command::Start { .. } => "start",
        Command::Restart { .. } => "restart",
        Command::Rm { .. } => "rm",
        Command::Fork { .. } => "fork",
        Command::Rebuild { .. } => "rebuild",
        Command::Volumes => "volumes",
        Command::Image(_) => "image",
        Command::Stat { .. } => "stat",
        Command::Url { .. } => "url",
        Command::Agent(_) => "agent",
        Command::Doctor => "doctor",
        Command::Completions { .. } => "completions",
        Command::Gateway(_) => "gateway",
    }
}

/// Parse a raw `$SSH_ORIGINAL_COMMAND` string and authorize it against the
/// gateway allowlist. Pure: no shell is ever invoked (the string is
/// `shlex`-split into an argv and handed straight to clap), so shell
/// metacharacters like `;` or `&&` are inert literal tokens — they can only
/// make parsing fail, never execute anything.
///
/// exe.dev-style direct attach: when the leading token is not a subcommand but
/// names an existing box (`has_box`), it is rewritten to `ssh <box> --
/// <rest>`, so `ssh host my-box` lands in the box. Real subcommands always win
/// — a box named `ls` stays reachable via `ssh host ssh ls`.
pub(crate) fn authorize(
    original: &str,
    has_box: &dyn Fn(&str) -> bool,
) -> std::result::Result<Command, GatewayReject> {
    if original.trim().is_empty() {
        return Err(GatewayReject::Empty);
    }
    let argv = shlex::split(original).ok_or(GatewayReject::Unshlexable)?;
    if argv.is_empty() {
        return Err(GatewayReject::Empty);
    }
    authorize_argv(argv, has_box).map(|(command, _)| command)
}

/// Split-argv form of [`authorize`], also returning the final argv (after any
/// direct-attach rewrite) so the SSH server can re-exec lilbox with it.
pub(crate) fn authorize_argv(
    mut argv: Vec<String>,
    has_box: &dyn Fn(&str) -> bool,
) -> std::result::Result<(Command, Vec<String>), GatewayReject> {
    if argv.is_empty() {
        return Err(GatewayReject::Empty);
    }
    if !is_subcommand(&argv[0]) && has_box(&argv[0]) {
        let name = argv.remove(0);
        let mut rewritten = vec!["ssh".to_string(), name];
        // `ssh <box> -- <cmd>` needs the separator; respect one the caller
        // already typed rather than doubling it.
        if !argv.is_empty() && argv[0] != "--" {
            rewritten.push("--".to_string());
        }
        rewritten.append(&mut argv);
        argv = rewritten;
    }
    let cli =
        Cli::try_parse_from(std::iter::once("lilbox".to_string()).chain(argv.iter().cloned()))
            .map_err(GatewayReject::Unparseable)?;
    if allowed(&cli.command) {
        Ok((cli.command, argv))
    } else {
        Err(GatewayReject::NotAllowed(command_label(&cli.command)))
    }
}

/// Is `token` a real subcommand (name or alias), per clap's own grammar?
fn is_subcommand(token: &str) -> bool {
    Cli::command()
        .get_subcommands()
        .any(|sub| sub.get_name() == token || sub.get_all_aliases().any(|a| a == token))
}

/// Process exit code for a rejected command — the contract an operator scripts
/// `ssh host ...` against. `Empty` (a bare login, or `--help`/`--version` which
/// clap surfaces as `Unparseable` with its own 0 code) is success; everything
/// else is a distinct non-zero.
fn reject_exit_code(reject: &GatewayReject) -> i32 {
    match reject {
        GatewayReject::Empty => 0,
        GatewayReject::Unshlexable | GatewayReject::NotAllowed(_) => 2,
        GatewayReject::Unparseable(error) => error.exit_code(),
    }
}

fn print_usage() {
    eprintln!(
        "lilbox gateway: run a lilbox command over SSH, e.g.\n  \
         ssh -t <host> new --tailscale            # create a box and drop into it\n  \
         ssh -t <host> my-box                     # attach to a running box\n  \
         ssh -t <host> new --tailscale --image node my-box\n  \
         ssh <host> ls\n  \
         ssh <host> rm <box>\n\
         allowed: new, ssh, ls, stat, url, logs, stop, start, restart, rm"
    );
}

pub(crate) async fn cmd_gateway(app: &App, args: crate::cli::GatewayArgs) -> Result<i32> {
    let gateway_config = app.config()?.gateway;
    let listen = args.listen.or(gateway_config.listen.clone());
    if let Some(listen) = listen {
        let keys = gateway_config
            .authorized_keys
            .unwrap_or_else(|| app.config_dir.join("gateway_authorized_keys"));
        return crate::gateway_server::run(
            app.db_path(),
            crate::gateway_server::Options { listen, keys },
        )
        .await;
    }
    let original = env::var("SSH_ORIGINAL_COMMAND").unwrap_or_default();
    let parsed = {
        let has_box = |name: &str| app.row(name).ok().flatten().is_some();
        authorize(&original, &has_box)
    };
    match parsed {
        Ok(Command::New(args)) => {
            let default_image = app.config()?.gateway.image;
            new::cmd_new(
                app,
                args,
                new::NewOptions {
                    attach: true,
                    default_image,
                    remote: true,
                },
            )
            .await
        }
        // Boxed to break the dispatch → cmd_gateway → dispatch type-level async
        // cycle; the allowlist forbids `Gateway`, so this never recurses at run
        // time.
        Ok(command) => Box::pin(dispatch(app, command)).await,
        Err(reject) => {
            match &reject {
                GatewayReject::Empty => print_usage(),
                GatewayReject::Unshlexable => {
                    eprintln!("gateway: could not parse command (unbalanced quotes)")
                }
                // Routes usage/errors to the right stream; for `--help`/`--version`
                // this prints them (and `reject_exit_code` yields 0).
                GatewayReject::Unparseable(error) => {
                    let _ = error.print();
                }
                GatewayReject::NotAllowed(name) => {
                    eprintln!("gateway: '{name}' is not permitted over the gateway")
                }
            }
            Ok(reject_exit_code(&reject))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No boxes exist: the plain parsing/allowlist behavior.
    fn authorize_no_boxes(input: &str) -> std::result::Result<Command, GatewayReject> {
        authorize(input, &|_| false)
    }

    #[test]
    fn new_with_positional_name_and_tailscale_is_allowed() {
        let command = authorize_no_boxes("new python --tailscale").unwrap();
        match command {
            Command::New(args) => {
                assert_eq!(args.name.as_deref(), Some("python"));
                assert!(args.tailnet);
                // The positional binds to the box NAME, not the image.
                assert!(args.image.is_none());
            }
            other => panic!("expected New, got {}", command_label(&other)),
        }
    }

    #[test]
    fn new_with_only_tailscale_flag_is_allowed() {
        let command = authorize_no_boxes("new --tailscale").unwrap();
        match command {
            Command::New(args) => {
                assert!(args.name.is_none());
                assert!(args.tailnet);
            }
            other => panic!("expected New, got {}", command_label(&other)),
        }
    }

    #[test]
    fn new_with_image_flag_override_is_allowed() {
        let command = authorize_no_boxes("new my-box --image node").unwrap();
        match command {
            Command::New(args) => {
                assert_eq!(args.name.as_deref(), Some("my-box"));
                assert_eq!(args.image.as_deref(), Some("node"));
            }
            other => panic!("expected New, got {}", command_label(&other)),
        }
    }

    #[test]
    fn read_only_commands_are_allowed() {
        for input in ["ls", "stat foo", "url foo", "logs foo"] {
            assert!(
                authorize_no_boxes(input).is_ok(),
                "{input} should be allowed"
            );
        }
    }

    #[test]
    fn lifecycle_commands_are_allowed() {
        for input in ["ssh foo", "stop foo", "start foo", "restart foo", "rm foo"] {
            assert!(
                authorize_no_boxes(input).is_ok(),
                "{input} should be allowed"
            );
        }
    }

    #[test]
    fn host_reaching_and_management_commands_are_denied() {
        for input in [
            "cp a b",
            "agent",
            "run",
            "exec foo -- ls",
            "image ls",
            "template add foo",
            "expose foo",
            "fork foo",
            "rebuild foo",
            "provision foo",
            "doctor",
            "completions bash",
            "gateway",
        ] {
            assert!(
                matches!(authorize_no_boxes(input), Err(GatewayReject::NotAllowed(_))),
                "{input} should be NotAllowed"
            );
        }
    }

    #[test]
    fn shell_metacharacters_do_not_inject() {
        // No shell is involved: `new;` is one literal token → unknown
        // subcommand, and the trailing `rm -rf /` are stray positionals. Either
        // way it fails to parse and is never dispatched as a valid `new`.
        assert!(matches!(
            authorize_no_boxes("new; rm -rf /"),
            Err(GatewayReject::Unparseable(_))
        ));
        assert!(matches!(
            authorize_no_boxes("new ; rm -rf /"),
            Err(GatewayReject::Unparseable(_))
        ));
    }

    #[test]
    fn empty_or_whitespace_command_is_empty() {
        assert!(matches!(authorize_no_boxes(""), Err(GatewayReject::Empty)));
        assert!(matches!(
            authorize_no_boxes("   "),
            Err(GatewayReject::Empty)
        ));
    }

    #[test]
    fn unbalanced_quote_is_unshlexable() {
        assert!(matches!(
            authorize_no_boxes("new \"unterminated"),
            Err(GatewayReject::Unshlexable)
        ));
    }

    #[test]
    fn unknown_flag_is_unparseable() {
        assert!(matches!(
            authorize_no_boxes("ls --bogus-flag"),
            Err(GatewayReject::Unparseable(_))
        ));
    }

    #[test]
    fn missing_required_positional_is_unparseable() {
        // `rm` requires a box name.
        assert!(matches!(
            authorize_no_boxes("rm"),
            Err(GatewayReject::Unparseable(_))
        ));
    }

    #[test]
    fn help_flag_is_unparseable_not_dispatched() {
        // clap surfaces --help as an error kind; it must not fall through to a
        // dispatchable command.
        assert!(matches!(
            authorize_no_boxes("new --help"),
            Err(GatewayReject::Unparseable(_))
        ));
    }

    #[test]
    fn existing_box_name_attaches_directly() {
        let command = authorize("my-box", &|name| name == "my-box").unwrap();
        match command {
            Command::Ssh(args) => {
                assert_eq!(args.name, "my-box");
                assert!(args.cmd.is_empty());
            }
            other => panic!("expected Ssh, got {}", command_label(&other)),
        }
    }

    #[test]
    fn box_name_with_trailing_tokens_runs_them_in_the_box() {
        let command = authorize("my-box uname -a", &|name| name == "my-box").unwrap();
        match command {
            Command::Ssh(args) => {
                assert_eq!(args.name, "my-box");
                assert_eq!(args.cmd, ["uname", "-a"]);
            }
            other => panic!("expected Ssh, got {}", command_label(&other)),
        }
    }

    #[test]
    fn explicit_separator_is_not_doubled() {
        let command = authorize("my-box -- uname -a", &|name| name == "my-box").unwrap();
        match command {
            Command::Ssh(args) => {
                assert_eq!(args.name, "my-box");
                assert_eq!(args.cmd, ["uname", "-a"]);
            }
            other => panic!("expected Ssh, got {}", command_label(&other)),
        }
    }

    #[test]
    fn subcommand_wins_over_same_named_box() {
        // A box named `ls` does not shadow the `ls` command.
        let command = authorize("ls", &|_| true).unwrap();
        assert!(matches!(command, Command::Ls { .. }));
    }

    #[test]
    fn unknown_name_without_a_box_stays_unparseable() {
        assert!(matches!(
            authorize("no-such-box", &|_| false),
            Err(GatewayReject::Unparseable(_))
        ));
    }

    // `Command` doesn't derive Debug, so `.unwrap_err()` won't compile; extract
    // the rejection by hand instead.
    fn reject(input: &str) -> GatewayReject {
        match authorize_no_boxes(input) {
            Ok(_) => panic!("expected '{input}' to be rejected"),
            Err(error) => error,
        }
    }

    #[test]
    fn empty_command_exits_zero() {
        assert_eq!(reject_exit_code(&reject("")), 0);
    }

    #[test]
    fn help_exits_zero() {
        // A remote `--help` should print help and succeed, not error out.
        assert_eq!(reject_exit_code(&reject("new --help")), 0);
    }

    #[test]
    fn not_allowed_exits_nonzero() {
        assert_eq!(reject_exit_code(&reject("cp a b")), 2);
    }

    #[test]
    fn unshlexable_exits_nonzero() {
        assert_eq!(reject_exit_code(&reject("new \"x")), 2);
    }

    #[test]
    fn unparseable_flag_exits_nonzero() {
        assert_ne!(reject_exit_code(&reject("ls --bogus")), 0);
    }
}
