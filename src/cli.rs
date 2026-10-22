use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::util::DEFAULT_IMAGE;

#[derive(Parser)]
#[command(
    name = "lilbox",
    about = "a lil exe.dev you run yourself - microVMs on your tailnet"
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Create a new persistent microVM.
    New(NewArgs),
    /// List boxes.
    Ls {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Reap boxes whose TTL has elapsed.
    Gc,
    /// List available templates.
    Templates,
    /// Manage user-installed templates.
    Template(TemplateArgs),
    /// Re-run a box template's setup script.
    Provision { name: String },
    /// Run a command in a box.
    Exec(ExecArgs),
    /// Open a shell (or run a command) in a box.
    Ssh(ExecArgs),
    /// Copy a file to or from a box (box side is NAME:/path).
    Cp { src: String, dst: String },
    /// Show a box's captured output.
    Logs(LogsArgs),
    /// Boot an ephemeral box, run a command, and discard it.
    Run(RunArgs),
    /// Publish a box over Tailscale HTTPS.
    Expose {
        name: String,
        #[arg(long)]
        public: bool,
    },
    /// Stop publishing a box.
    Unexpose { name: String },
    /// Join a box to the tailnet as its own node.
    Join {
        name: String,
        /// ACL tag to advertise (defaults to the [tailscale] tag).
        #[arg(long)]
        tailnet_tag: Option<String>,
    },
    /// Log a box out of the tailnet.
    Leave { name: String },
    /// Stop a running box.
    Stop { name: String },
    /// Start a stopped box.
    Start { name: String },
    /// Restart a box.
    Restart { name: String },
    /// Remove a box.
    Rm {
        name: String,
        #[arg(long)]
        keep_data: bool,
    },
    /// Snapshot a box and boot a clone.
    Fork {
        name: String,
        newname: Option<String>,
        /// Leave the tailnet first when the box is joined, instead of refusing.
        #[arg(long)]
        force: bool,
    },
    /// Recreate a box on an image while preserving its home volume.
    Rebuild {
        name: String,
        #[arg(long)]
        image: Option<String>,
    },
    /// List persistent home volumes.
    Volumes,
    /// Manage the embedded microsandbox image cache.
    Image(ImageArgs),
    /// Show detailed box information.
    Stat { name: String },
    /// Print a box's published URL.
    Url { name: String },
    /// Run Claude Code in a microVM against a mounted workspace.
    Agent(AgentArgs),
    /// SSH gateway: with `--listen` (or `[gateway] listen` in config), act as
    /// a standalone SSH server — the exe.dev-style UX: `ssh <box>@host`
    /// attaches, `ssh new@host` creates, `ssh host ls` runs allowlisted
    /// commands. Without it, act as an SSH forced command dispatching
    /// $SSH_ORIGINAL_COMMAND through a strict allowlist (wired as
    /// `command="lilbox gateway"` in authorized_keys).
    Gateway(GatewayArgs),
    /// Check the local runtime and Tailscale installation.
    Doctor,
    /// Generate a shell completion script.
    Completions {
        /// Shell to generate completions for.
        shell: clap_complete::Shell,
    },
}

#[derive(Args)]
pub(crate) struct ImageArgs {
    #[command(subcommand)]
    pub(crate) command: ImageCommand,
}

#[derive(Subcommand)]
pub(crate) enum ImageCommand {
    /// Import an OCI or Docker archive into the image cache.
    Load {
        archive: PathBuf,
        #[arg(short, long)]
        tag: String,
    },
    /// List cached images.
    Ls,
    /// Pull a base image, append a marker layer, and load it under a new tag.
    Overlay {
        base: String,
        #[arg(long)]
        tag: String,
    },
    /// Pull a base image, append a Tailscale layer, and load it under a new tag.
    Tailscalify {
        base: String,
        #[arg(long)]
        tag: String,
    },
}

#[derive(Args)]
pub(crate) struct TemplateArgs {
    #[command(subcommand)]
    pub(crate) command: TemplateCommand,
}

#[derive(Subcommand)]
pub(crate) enum TemplateCommand {
    /// Install a template from a git URL or a local directory.
    Add {
        source: String,
        /// Name to install the template under (default: derived from source).
        #[arg(long)]
        name: Option<String>,
        /// Overwrite an existing template with the same name.
        #[arg(long)]
        force: bool,
    },
    /// Remove an installed user template.
    Remove { name: String },
}

#[derive(Args)]
pub(crate) struct NewArgs {
    pub(crate) name: Option<String>,
    #[arg(long)]
    pub(crate) template: Option<String>,
    #[arg(long)]
    pub(crate) image: Option<String>,
    #[arg(long)]
    pub(crate) port: Option<u16>,
    #[arg(long)]
    pub(crate) cpus: Option<u8>,
    #[arg(long)]
    pub(crate) memory: Option<String>,
    #[arg(long)]
    pub(crate) rebuild: bool,
    #[arg(long)]
    pub(crate) no_persist: bool,
    #[arg(long)]
    pub(crate) volume: Option<String>,
    #[arg(long)]
    pub(crate) ttl: Option<String>,
    #[arg(long)]
    pub(crate) idle_timeout: Option<String>,
    /// Tailnet ACL tag to advertise when joining (default: tag:lilbox-vm).
    /// Implies --tailnet.
    #[arg(long)]
    pub(crate) tailnet_tag: Option<String>,
    /// Join the tailnet (auto-builds a cached tailnet-capable variant of the
    /// image if needed). Implied by --tailnet-tag, or by [tailscale] auto = true.
    #[arg(long, visible_alias = "tailscale")]
    pub(crate) tailnet: bool,
    /// Attach to a guest shell once the box is up. Used by the gateway server
    /// for interactive sessions; local `lilbox ssh <box>` is the normal path.
    #[arg(long, hide = true)]
    pub(crate) attach: bool,
}

#[derive(Args)]
pub(crate) struct GatewayArgs {
    /// Listen as a standalone SSH server (e.g. 0.0.0.0:2222) instead of
    /// reading $SSH_ORIGINAL_COMMAND. Defaults to [gateway] listen in config.
    #[arg(long)]
    pub(crate) listen: Option<String>,
}

#[derive(Args)]
pub(crate) struct ExecArgs {
    pub(crate) name: String,
    #[arg(last = true)]
    pub(crate) cmd: Vec<String>,
}

#[derive(Args)]
pub(crate) struct RunArgs {
    #[arg(long, default_value = DEFAULT_IMAGE)]
    pub(crate) image: String,
    #[arg(long)]
    pub(crate) ttl: Option<String>,
    #[arg(last = true)]
    pub(crate) cmd: Vec<String>,
}

#[derive(Args)]
pub(crate) struct LogsArgs {
    pub(crate) name: String,
    /// Keep streaming newly appended output.
    #[arg(short, long)]
    pub(crate) follow: bool,
    /// Show only the last N captured chunks.
    #[arg(long)]
    pub(crate) tail: Option<usize>,
    /// Comma-separated stdout, stderr, output, system, or all.
    #[arg(long)]
    pub(crate) source: Option<String>,
}

#[derive(Args)]
pub(crate) struct AgentArgs {
    pub(crate) name: Option<String>,
    #[arg(long, default_value = DEFAULT_IMAGE)]
    pub(crate) image: String,
    #[arg(long, conflicts_with = "clone")]
    pub(crate) workspace: Option<PathBuf>,
    #[arg(long)]
    pub(crate) clone: Option<String>,
    #[arg(long)]
    pub(crate) agents_file: Option<PathBuf>,
    #[arg(long, default_value = "ANTHROPIC_API_KEY")]
    pub(crate) key_env: String,
    #[arg(long, default_value = "api.anthropic.com")]
    pub(crate) key_host: String,
    /// Don't inherit the host's Claude login into the box; fall back to ANTHROPIC_API_KEY injection.
    #[arg(long)]
    pub(crate) no_claude_config: bool,
    /// Extra KEY=VAL environment for the guest agent exec. Transient: passed to
    /// the exec call only, never persisted in the sandbox config (the herdr
    /// plugin uses this to forward pane identity, #131).
    #[arg(long = "env", value_name = "KEY=VAL")]
    pub(crate) env: Vec<String>,
    #[arg(last = true)]
    pub(crate) task: Vec<String>,
}
