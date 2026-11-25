use std::{
    collections::HashMap,
    io::{self, IsTerminal, Read, Write},
    process::Command as ProcessCommand,
    thread,
};

use anyhow::{Context, Result, bail};
use microsandbox::{
    ExecEvent, MicrosandboxError, Sandbox, SecretSource,
    sandbox::{SandboxBuilder, SandboxStatus, SecretBuilder},
};

use crate::app::App;
use crate::cli::{ExecArgs, RunArgs};
use crate::tailscale::tailscale_ssh_args;
use crate::util::{parse_duration, random_name};

const DEFAULT_HOME: &str = "/root";

pub(crate) fn status_name(status: SandboxStatus) -> String {
    format!("{status:?}").to_ascii_lowercase()
}

pub(crate) async fn statuses() -> Result<HashMap<String, SandboxStatus>> {
    let mut result = HashMap::new();
    let mut cursor = None;
    loop {
        let requested = cursor.clone();
        let page = Sandbox::list_with(|b| {
            let b = b.limit(100);
            if let Some(c) = requested {
                b.cursor(c)
            } else {
                b
            }
        })
        .await?;
        for handle in page.sandboxes {
            result.insert(handle.name().to_string(), handle.status_snapshot());
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    Ok(result)
}

pub(crate) async fn connect_box(app: &App, name: &str, auto_resume: bool) -> Result<Sandbox> {
    let row = app.require_row(name)?;
    let handle = Sandbox::get(name)
        .await
        .with_context(|| format!("microsandbox has no box named '{name}'"))?;
    match handle.status_snapshot() {
        SandboxStatus::Running | SandboxStatus::Draining => Ok(handle.connect().await?),
        _ if auto_resume && row.stopped_reason.as_deref() != Some("user") => {
            println!("resuming {name} ...");
            let sandbox = handle.start_detached().await?;
            app.db
                .execute("UPDATE boxes SET stopped_reason=NULL WHERE name=?1", [name])?;
            Ok(sandbox)
        }
        _ => bail!("box '{name}' is stopped - start it with: lilbox start {name}"),
    }
}

/// `exec_env` is transient: applied to this exec/attach call only, never
/// written into the sandbox's persisted config.
pub(crate) async fn run_guest(
    sandbox: &Sandbox,
    command: &[String],
    cwd: Option<&str>,
    exec_env: &[(String, String)],
) -> Result<i32> {
    if command.is_empty() {
        bail!("nothing to run");
    }
    let cmd = &command[0];
    let args = &command[1..];
    let stdin_is_terminal = io::stdin().is_terminal();
    if stdin_is_terminal && io::stdout().is_terminal() {
        return Ok(sandbox
            .attach_with(cmd, |a| {
                let a = a.args(args.iter().cloned());
                let a = exec_env
                    .iter()
                    .fold(a, |a, (k, v)| a.env(k.as_str(), v.as_str()));
                if let Some(cwd) = cwd { a.cwd(cwd) } else { a }
            })
            .await?);
    }
    let mut handle = sandbox
        .exec_stream_with(cmd, |e| {
            let e = e.args(args.iter().cloned());
            let e = exec_env
                .iter()
                .fold(e, |e, (k, v)| e.env(k.as_str(), v.as_str()));
            let e = if stdin_is_terminal {
                e.stdin_null()
            } else {
                e.stdin_pipe()
            };
            if let Some(cwd) = cwd { e.cwd(cwd) } else { e }
        })
        .await?;
    let stdin_sink = handle.take_stdin();
    if !stdin_is_terminal && stdin_sink.is_none() {
        bail!("microsandbox did not provide a stdin pipe");
    }
    let (input_tx, mut input_rx) = tokio::sync::mpsc::channel(1);
    let mut stdin_open = !stdin_is_terminal;
    if stdin_open {
        thread::spawn(move || {
            let mut stdin = io::stdin().lock();
            let mut buffer = [0_u8; 16 * 1024];
            loop {
                match stdin.read(&mut buffer) {
                    Ok(0) => {
                        let _ = input_tx.blocking_send(Ok(Vec::new()));
                        break;
                    }
                    Ok(count) => {
                        if input_tx
                            .blocking_send(Ok(buffer[..count].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = input_tx.blocking_send(Err(error));
                        break;
                    }
                }
            }
        });
    }

    loop {
        tokio::select! {
            event = handle.recv() => match event {
                Some(ExecEvent::Started { .. }) => {}
                Some(ExecEvent::Stdout(data)) => {
                    let mut stdout = io::stdout().lock();
                    stdout.write_all(&data)?;
                    stdout.flush()?;
                }
                Some(ExecEvent::Stderr(data)) => {
                    let mut stderr = io::stderr().lock();
                    stderr.write_all(&data)?;
                    stderr.flush()?;
                }
                Some(ExecEvent::Exited { code }) => return Ok(code),
                Some(ExecEvent::Failed(error)) => {
                    return Err(MicrosandboxError::ExecFailed(error).into());
                }
                Some(ExecEvent::StdinError(error)) => {
                    eprintln!("warning: guest stdin closed: {error:?}");
                    stdin_open = false;
                }
                None => bail!("exec session ended without an exit event"),
            },
            input = input_rx.recv(), if stdin_open => {
                match input {
                    Some(Ok(data)) if data.is_empty() => {
                        if let Err(error) = stdin_sink.as_ref().unwrap().close().await {
                            eprintln!("warning: could not close guest stdin: {error}");
                        }
                        stdin_open = false;
                    }
                    Some(Ok(data)) => {
                        if let Err(error) = stdin_sink.as_ref().unwrap().write(data).await {
                            eprintln!("warning: could not write guest stdin: {error}");
                            stdin_open = false;
                        }
                    }
                    Some(Err(error)) => {
                        eprintln!("warning: could not read host stdin: {error}");
                        stdin_open = false;
                    }
                    None => stdin_open = false,
                }
            }
        }
    }
}

pub(crate) async fn exec(app: &App, args: ExecArgs) -> Result<i32> {
    if args.cmd.is_empty() {
        bail!("nothing to run - usage: lilbox exec NAME -- <cmd>");
    }
    run_guest(
        &connect_box(app, &args.name, true).await?,
        &args.cmd,
        None,
        &[],
    )
    .await
}

pub(crate) async fn ssh(app: &App, args: ExecArgs) -> Result<i32> {
    let row = app.require_row(&args.name)?;
    if let (Some(node), Some(ts)) = (row.tailscale_node.as_deref(), app.tailscale.as_deref()) {
        let status = ProcessCommand::new(ts)
            .args(tailscale_ssh_args(node, &args.cmd))
            .status()
            .with_context(|| format!("could not run '{}'", ts.display()))?;
        return Ok(status.code().unwrap_or(1));
    }

    // microsandbox's interactive attach re-opens stdin's tty by path to get an
    // independent nonblocking fd. Under Tailscale SSH the pty slave stays
    // root-owned (mode 600), so that open fails with EACCES. Re-exec under a
    // fresh script(1) pty — allocated as our user — so the re-open works.
    if args.cmd.is_empty() && stdin_tty_reopen_blocked() {
        return reexec_under_pty_shim(&args.name);
    }

    let sandbox = connect_box(app, &args.name, true).await?;
    if args.cmd.is_empty() {
        return Ok(sandbox.attach_shell().await?);
    }
    run_guest(&sandbox, &args.cmd, None, &[]).await
}

/// True when stdin is a tty we cannot re-open by path (e.g. a Tailscale SSH
/// session pty). Non-tty stdin is fine — the attach only re-opens terminals.
#[cfg(unix)]
fn stdin_tty_reopen_blocked() -> bool {
    use std::os::fd::AsRawFd;
    if std::env::var_os("LILBOX_PTY_SHIM").is_some() {
        return false; // already re-exec'd under the shim; never loop
    }
    if !io::stdin().is_terminal() {
        return false;
    }
    let mut buf = [0u8; 1024];
    let rc =
        unsafe { libc::ttyname_r(io::stdin().as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return false; // can't resolve; let the attach surface its own error
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let Ok(path) = std::str::from_utf8(&buf[..end]) else {
        return false;
    };
    std::fs::File::open(path).is_err()
}

/// Re-run `lilbox ssh <name>` under script(1), which allocates a pty owned by
/// our user. SHELL must point at a real shell: under the login-shell gateway
/// deployment the user's login shell *is* lilbox, and script(1) spawns $SHELL.
#[cfg(unix)]
fn reexec_under_pty_shim(name: &str) -> Result<i32> {
    let exe = std::env::current_exe().context("resolve current exe for the pty shim")?;
    let quoted = shlex::try_quote(name).context("box name is not shell-quotable")?;
    let inner = format!("{} ssh {}", exe.display(), quoted);
    #[cfg(target_os = "macos")]
    let argv = ["-q", "/dev/null", &inner];
    #[cfg(not(target_os = "macos"))]
    let argv = ["-qefc", &inner, "/dev/null"];
    let status = ProcessCommand::new("script")
        .args(argv)
        .env("SHELL", "/bin/sh")
        .env("LILBOX_PTY_SHIM", "1")
        .status()
        .context("could not run 'script' (util-linux) for the pty shim")?;
    Ok(status.code().unwrap_or(1))
}

pub(crate) async fn run(app: &App, args: RunArgs) -> Result<i32> {
    let name = random_name(app)?;
    let mut builder = Sandbox::builder(&name)
        .image(args.image.as_str())
        .ephemeral(true);
    if let Some(ttl) = args.ttl {
        builder = builder.max_duration(parse_duration(&ttl)?);
    }
    let sandbox = builder.create().await?;
    let code = if args.cmd.is_empty() {
        sandbox.attach_shell().await?
    } else {
        run_guest(&sandbox, &args.cmd, None, &[]).await?
    };
    sandbox.stop().await?;
    Ok(code)
}

pub(crate) struct SandboxSettings<'a> {
    pub(crate) name: &'a str,
    pub(crate) image: &'a str,
    pub(crate) host_port: u16,
    pub(crate) guest_port: u16,
    pub(crate) cpus: Option<u8>,
    pub(crate) memory: Option<u32>,
    pub(crate) idle_timeout: Option<u64>,
    pub(crate) volume: Option<&'a str>,
}

pub(crate) fn configure_builder(
    settings: SandboxSettings<'_>,
) -> microsandbox::sandbox::SandboxBuilder {
    let mut builder = Sandbox::builder(settings.name)
        .image(settings.image)
        .port(settings.host_port, settings.guest_port)
        .detached(true)
        .label("dev.lilbox.managed", "true");
    if let Some(cpus) = settings.cpus {
        builder = builder.cpus(cpus);
    }
    if let Some(memory) = settings.memory {
        builder = builder.memory(memory);
    }
    if let Some(seconds) = settings.idle_timeout {
        builder = builder.idle_timeout(seconds);
    }
    if let Some(volume) = settings.volume {
        builder = builder.volume(DEFAULT_HOME, |m| {
            m.named_with(volume, |v| v.ensure_exists())
        });
    }
    builder
}

fn secret_shape(secret: SecretBuilder, env: &str, host: &str) -> SecretBuilder {
    secret.env(env).allow_host(host)
}

pub(crate) fn with_secret_env(builder: SandboxBuilder, env: &str, host: &str) -> SandboxBuilder {
    builder.secret(|secret| {
        secret_shape(secret, env, host).source(SecretSource::Env {
            var: env.to_owned(),
        })
    })
}

pub(crate) async fn stop_and_remove(name: &str) -> Result<()> {
    if let Ok(handle) = Sandbox::get(name).await {
        let _ = handle.stop().await;
    }
    match Sandbox::remove(name).await {
        Ok(()) | Err(MicrosandboxError::SandboxNotFound(_)) => Ok(()),
        Err(err) => Err(err).with_context(|| format!("could not remove '{name}'")),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pty_shim_not_triggered_without_terminal() {
        // Test harness stdin is not a tty, so the shim must stay off.
        assert!(!super::stdin_tty_reopen_blocked());
    }

    #[test]
    fn pty_shim_env_guard_wins() {
        // The re-exec'd child must never re-trigger the shim.
        unsafe { std::env::set_var("LILBOX_PTY_SHIM", "1") };
        assert!(!super::stdin_tty_reopen_blocked());
        unsafe { std::env::remove_var("LILBOX_PTY_SHIM") };
    }
}
