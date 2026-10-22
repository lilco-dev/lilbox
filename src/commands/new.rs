use std::env;
use std::io::IsTerminal;

use anyhow::{Context, Result, anyhow, bail};
use chrono::Local;
use rusqlite::params;

use crate::app::App;
use crate::cli::NewArgs;
use crate::overlay;
use crate::provision::{build_template_image, provision_sandbox};
use crate::sandbox::{SandboxSettings, configure_builder, stop_and_remove};
use crate::tailscale::{
    DEFAULT_AUTH_KEY_ENV, JoinMode, box_display_url, is_valid_env_var_name, join_box,
    mint_ephemeral_key, require_auth_key, resolve_join_mode, resolve_tag, validate_tag,
    wants_tailnet,
};
use crate::util::{
    DEFAULT_GUEST_PORT, DEFAULT_IMAGE, alloc_host_port, now, or_cleanup, parse_duration,
    parse_memory, random_name,
};

/// Internal knobs for `cmd_new` that aren't user-facing CLI flags. The local
/// `lilbox new` path uses the defaults; the gateway path sets `attach` and the
/// `[gateway] image` default (see `commands::gateway`).
#[derive(Default)]
pub(crate) struct NewOptions {
    /// After the box is up, attach this process's stdio to a guest shell
    /// (falls back to printing connection info when stdin isn't a TTY).
    pub(crate) attach: bool,
    /// Image to boot when neither `--image` nor a template picks one, taking
    /// precedence over the global `[image]` config default. Gateway-only.
    pub(crate) default_image: Option<String>,
    /// Running under `lilbox gateway`: connection hints print the remote
    /// (`ssh <host> <box>`) form, not the local CLI form.
    pub(crate) remote: bool,
}

/// How `cmd_new` should source the box image, in precedence order. Pure so the
/// ordering can be unit-tested without a docker/image backend; the async
/// dockerfile build stays in the caller.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ImageChoice {
    /// `--image`, a template's manifest image, or a resolved default.
    Named(String),
    /// A template with a Dockerfile: build it (async, in the caller).
    BuildDockerfile,
}

/// Resolve the image source: `--image` > template manifest image > template
/// Dockerfile build > gateway default > `[image]` config > `DEFAULT_IMAGE`.
/// `default_image` is the gateway-only tier and is `None` on the local path,
/// so it never leaks into a plain `lilbox new`.
pub(crate) fn resolve_image_choice(
    arg_image: Option<String>,
    template_image: Option<String>,
    template_has_dockerfile: bool,
    default_image: Option<String>,
    config_image: Option<String>,
) -> ImageChoice {
    if let Some(image) = arg_image {
        ImageChoice::Named(image)
    } else if let Some(image) = template_image {
        ImageChoice::Named(image)
    } else if template_has_dockerfile {
        ImageChoice::BuildDockerfile
    } else if let Some(image) = default_image {
        ImageChoice::Named(image)
    } else {
        ImageChoice::Named(config_image.unwrap_or_else(|| DEFAULT_IMAGE.into()))
    }
}

pub(crate) async fn cmd_new(app: &App, args: NewArgs, opts: NewOptions) -> Result<i32> {
    let name = match args.name {
        Some(name) => name,
        None => random_name(app)?,
    };
    if app.row(&name)?.is_some() {
        bail!("box '{name}' already exists");
    }
    let config = app.config()?;
    let mut tag = resolve_tag(args.tailnet_tag.as_deref(), config.tailscale.tag.as_deref());
    // Tailnet join is opt-in: a key alone in the environment no longer
    // triggers anything. It fires only on explicit intent (--tailnet,
    // --tailnet-tag, or [tailscale] auto = true) plus a resolvable credential.
    let want_tailnet = wants_tailnet(
        args.tailnet,
        args.tailnet_tag.as_deref(),
        config.tailscale.auto,
    );
    let mut key_env = DEFAULT_AUTH_KEY_ENV.to_string();
    let mut minted_key: Option<String> = None;
    let mut joins_tailnet = false;
    if want_tailnet {
        let mode = resolve_join_mode(&config.tailscale, args.tailnet_tag.as_deref(), |name| {
            env::var(name).ok()
        });
        joins_tailnet = match mode {
            JoinMode::Mint {
                tag: mint_tag,
                client_id,
                client_secret,
            } => {
                tag = mint_tag;
                // A malformed tag from config must not abort `new` (never fail on a
                // tailnet problem) — warn and skip the join. An explicit --tailnet-tag
                // is still hard-validated below for fast feedback.
                if let Err(error) = validate_tag(&tag) {
                    eprintln!("warning: {error}; skipping tailnet join");
                    false
                } else {
                    match mint_ephemeral_key(&client_id, &client_secret, &tag, &name).await {
                        Ok(key) => {
                            minted_key = Some(key);
                            true
                        }
                        Err(error) => {
                            eprintln!(
                                "warning: could not mint tailnet auth key: {error}; skipping tailnet join"
                            );
                            false
                        }
                    }
                }
            }
            JoinMode::StaticEnv { key_env: env_name } => {
                key_env = env_name;
                if !is_valid_env_var_name(&key_env) {
                    eprintln!(
                        "warning: tailscale authKeyEnv '{key_env}' is not a valid environment variable name; skipping tailnet join"
                    );
                    false
                } else {
                    require_auth_key(env::var(&key_env).ok(), &key_env).is_ok()
                }
            }
            JoinMode::Skip => {
                eprintln!(
                    "note: --tailnet requested but no auth key set (TS_AUTHKEY / [tailscale] oauthClientId) — booting '{name}' without joining the tailnet"
                );
                false
            }
        };
    }
    // Only validate the tag when it will actually be used to join: an explicitly
    // passed --tailnet-tag is still validated for fast feedback, but a malformed
    // tag left over in config shouldn't abort `new` when there's no auth key to
    // join with.
    if joins_tailnet || args.tailnet_tag.is_some() {
        validate_tag(&tag)?;
    }
    let template = args
        .template
        .as_deref()
        .map(|n| app.template(n))
        .transpose()?;
    let mut image = match resolve_image_choice(
        args.image,
        template.as_ref().and_then(|t| t.manifest.image.clone()),
        template.as_ref().is_some_and(|t| t.dockerfile),
        opts.default_image,
        config.image.clone(),
    ) {
        ImageChoice::Named(image) => image,
        ImageChoice::BuildDockerfile => {
            build_template_image(template.as_ref().unwrap(), args.rebuild).await?
        }
    };
    let is_tailnet_capable = image == "lilbox-box" || image.starts_with("lilbox/tailnet/");
    if joins_tailnet && !is_tailnet_capable {
        match overlay::ensure_tailnet_image(&image, args.rebuild).await {
            Ok(t) => image = t,
            Err(e) => eprintln!(
                "warning: could not build a tailnet-capable image for '{image}': {e:#}; booting the base as-is"
            ),
        }
    } else if !want_tailnet && is_tailnet_capable {
        eprintln!("note: '{image}' is tailnet-capable; pass --tailnet to join the tailnet");
    }
    let guest_port = args
        .port
        .or_else(|| template.as_ref().and_then(|t| t.manifest.port))
        .or(config.port)
        .unwrap_or(DEFAULT_GUEST_PORT);
    let mut cpus = args
        .cpus
        .or_else(|| template.as_ref().and_then(|t| t.manifest.cpus))
        .or(config.cpus);
    let mut memory_text = args
        .memory
        .or_else(|| template.as_ref().and_then(|t| t.manifest.memory.clone()))
        .or(config.memory);
    if let (Some(value), Some(max)) = (cpus, config.max_cpus)
        && value > max
    {
        eprintln!("warning: cpus {value} exceeds max_cpus {max}; clamping");
        cpus = Some(max);
    }
    if let (Some(value), Some(max)) = (&memory_text, &config.max_memory)
        && parse_memory(value)? > parse_memory(max)?
    {
        eprintln!("warning: memory {value} exceeds max_memory {max}; clamping");
        memory_text = Some(max.clone());
    }
    let memory = memory_text.as_deref().map(parse_memory).transpose()?;
    let ttl = args.ttl.or(config.ttl);
    let expires = ttl
        .as_deref()
        .map(parse_duration)
        .transpose()?
        .map(|s| now() + s as i64);
    let idle = args
        .idle_timeout
        .or(config.idle_timeout)
        .as_deref()
        .map(parse_duration)
        .transpose()?;
    let volume = if args.no_persist {
        None
    } else {
        Some(args.volume.unwrap_or_else(|| format!("lilbox-{name}-home")))
    };
    let host_port = alloc_host_port()?;
    println!(
        "booting {image} microVM as {name}{} ...",
        if volume.is_some() {
            " (persistent)"
        } else {
            ""
        }
    );
    let builder = configure_builder(SandboxSettings {
        name: &name,
        image: &image,
        host_port,
        guest_port,
        cpus,
        memory,
        idle_timeout: idle,
        volume: volume.as_deref(),
    });

    // Register termination handlers BEFORE the VM exists, so a (rare) signal
    // registration failure aborts here rather than orphaning a box. On the
    // gateway path the common interruption is the SSH client disconnecting,
    // which arrives as SIGHUP/SIGTERM (not SIGINT) — cover all three, or the
    // atomic teardown below wouldn't fire for the case that most needs it.
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sighup = signal(SignalKind::hangup())?;

    let sandbox = builder.create_detached().await.with_context(|| {
        format!(
            "could not create box '{name}' from image '{image}' \
             (if '{image}' is a locally-built image, load it first — \
             e.g. images/<name>/build.sh — and check `lilbox image ls`)"
        )
    })?;

    // Provision against the handle we already hold (never `connect_box`, which
    // needs a DB row) so the row insert below stays the atomic commit point: a
    // box is recorded only once it's fully joined and provisioned. Any failure
    // or termination signal in this window tears the half-built VM down.
    // Mirrors the agent-box pattern (#106).
    let provision = async {
        let mut node: Option<String> = None;
        if joins_tailnet {
            node = join_tailnet(
                &sandbox,
                &tag,
                &key_env,
                &mut minted_key,
                &name,
                guest_port,
                &image,
            )
            .await;
        }
        if let Some(template) = &template {
            provision_sandbox(app, &sandbox, &name, template).await?;
        }
        Ok::<Option<String>, anyhow::Error>(node)
    };
    tokio::pin!(provision);
    let outcome = tokio::select! {
        r = &mut provision => r,
        _ = tokio::signal::ctrl_c() => Err(anyhow!("interrupted before the box finished provisioning")),
        _ = sigterm.recv() => Err(anyhow!("terminated before the box finished provisioning")),
        _ = sighup.recv() => Err(anyhow!("disconnected before the box finished provisioning")),
    };
    let node = or_cleanup(outcome, || stop_and_remove(&name)).await?;

    // Commit point. Guard the row write too: if it fails (e.g. SQLITE_BUSY, or a
    // duplicate name that appeared in the widened window since the existence
    // check), the VM is already built, so tear it down rather than orphan it.
    let insert = app.db.execute(
        "INSERT INTO boxes(name,image,guest_port,host_port,created,template,volume,expires,tailscale_node) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![name, image, guest_port, host_port, Local::now().format("%Y-%m-%d %H:%M:%S").to_string(), template.as_ref().map(|t| &t.name), volume, expires, node],
    ).map(|_| ()).map_err(anyhow::Error::from);
    or_cleanup(insert, || stop_and_remove(&name)).await?;
    println!("box {name} is up ({image}, guest :{guest_port})");

    if opts.attach {
        // Attach AFTER the commit point and OUTSIDE the interrupt select above,
        // so Ctrl-C in the interactive session ends the shell rather than
        // reaping a fully-provisioned box.
        if std::io::stdin().is_terminal() {
            return Ok(sandbox.attach_shell().await?);
        }
        // No TTY (scripted `ssh host new ... < /dev/null`): can't attach, so
        // print how to reach the box instead.
        match box_display_url(node.as_deref(), None) {
            Some(url) => println!("{name} ready at {url}"),
            None if opts.remote => {
                println!("{name} ready (attach with: {})", remote_ssh_hint(&name))
            }
            None => println!("{name} ready (reach it with: lilbox ssh {name})"),
        }
        return Ok(0);
    }

    println!("  lilbox exec {name} -- <cmd>\n  lilbox ssh {name}\n  lilbox expose {name}");
    Ok(0)
}

/// Copy-pasteable remote attach hint for the gateway path. sshd sets
/// `SSH_CONNECTION` to "client_ip client_port server_ip server_port", which
/// pins the address *and* port the client actually reached us on; without it
/// (tests, non-ssh invocation) fall back to the generic form.
fn remote_ssh_hint(name: &str) -> String {
    let connection = env::var("SSH_CONNECTION").unwrap_or_default();
    let fields: Vec<&str> = connection.split_whitespace().collect();
    match fields.as_slice() {
        [_, _, server_ip, server_port] => format!("ssh -p {server_port} {server_ip} {name}"),
        _ => format!("ssh <host> {name}"),
    }
}

/// Join a freshly-created box to the tailnet, returning its MagicDNS node name
/// on success. Never fails the caller: every problem is warned and yields
/// `None`, preserving `new`'s "a tailnet hiccup must not abort the box" rule.
///
/// The join itself lives in [`join_box`], shared with `lilbox join`; this
/// wrapper exists only to convert its errors into warnings. `lilbox join` keeps
/// them fatal.
async fn join_tailnet(
    sandbox: &microsandbox::Sandbox,
    tag: &str,
    key_env: &str,
    minted_key: &mut Option<String>,
    name: &str,
    guest_port: u16,
    image: &str,
) -> Option<String> {
    let Some(auth_key) = minted_key.take().or_else(|| env::var(key_env).ok()) else {
        eprintln!(
            "warning: could not resolve tailnet auth key for '{name}'; skipping tailnet join"
        );
        return None;
    };
    match join_box(sandbox, tag, key_env, auth_key, name, guest_port, image).await {
        Ok(node) => Some(node),
        Err(error) => {
            eprintln!("warning: {error:#}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_image_arg_flag_wins_over_everything() {
        let choice = resolve_image_choice(
            Some("node".into()),
            Some("ruby".into()),
            true,
            Some("gw".into()),
            Some("cfg".into()),
        );
        assert_eq!(choice, ImageChoice::Named("node".into()));
    }

    #[test]
    fn resolve_image_template_wins_when_no_arg() {
        let choice = resolve_image_choice(
            None,
            Some("ruby".into()),
            true,
            Some("gw".into()),
            Some("cfg".into()),
        );
        assert_eq!(choice, ImageChoice::Named("ruby".into()));
    }

    #[test]
    fn resolve_image_builds_dockerfile_when_no_arg_or_template_image() {
        let choice = resolve_image_choice(None, None, true, Some("gw".into()), Some("cfg".into()));
        assert_eq!(choice, ImageChoice::BuildDockerfile);
    }

    #[test]
    fn resolve_image_gateway_default_wins_over_config() {
        let choice = resolve_image_choice(None, None, false, Some("gw".into()), Some("cfg".into()));
        assert_eq!(choice, ImageChoice::Named("gw".into()));
    }

    #[test]
    fn resolve_image_config_used_when_no_gateway_default() {
        let choice = resolve_image_choice(None, None, false, None, Some("cfg".into()));
        assert_eq!(choice, ImageChoice::Named("cfg".into()));
    }

    #[test]
    fn resolve_image_falls_back_to_default_when_nothing_set() {
        let choice = resolve_image_choice(None, None, false, None, None);
        assert_eq!(choice, ImageChoice::Named(DEFAULT_IMAGE.into()));
    }

    #[test]
    fn resolve_image_gateway_tier_absent_on_local_path() {
        // The local `lilbox new` caller passes default_image: None, so the
        // gateway tier can never divert a plain `new` — it falls to config.
        let choice = resolve_image_choice(None, None, false, None, Some("cfg".into()));
        assert_eq!(choice, ImageChoice::Named("cfg".into()));
    }
}
