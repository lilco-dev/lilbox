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

    let sandbox = connect_box(app, &args.name, true).await?;
    if args.cmd.is_empty() {
        return Ok(sandbox.attach_shell().await?);
    }
    run_guest(&sandbox, &args.cmd, None, &[]).await
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
    with_herdr_vsock(builder, herdr_socket_path_from_env())
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

/// Guest-side vsock port that forwards the host's herdr control socket.
/// Guests connect to AF_VSOCK CID 2 on this port (spike: #127).
pub(crate) const HERDR_VSOCK_PORT: u32 = 47100;

/// Decide whether to route the host's herdr control socket into the box.
/// Returns the socket path + guest port only when the env var is set and the
/// path is a live unix socket; anything else (unset, missing, not a socket)
/// means "herdr isn't driving" and the box must provision identically.
pub(crate) fn herdr_vsock_route(
    socket_path: Option<std::path::PathBuf>,
) -> Option<(std::path::PathBuf, u32)> {
    let path = socket_path?;
    let meta = std::fs::metadata(&path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if !meta.file_type().is_socket() {
            return None;
        }
    }
    Some((path, HERDR_VSOCK_PORT))
}

/// Wire the route into a builder when herdr is driving this invocation.
/// Best-effort by construction: with no route decision the builder passes
/// through untouched, so provisioning can never fail because of this.
/// Scope: `new`, `rebuild` (both via `configure_builder`) and `agent` get the
/// route; `fork` and ad-hoc `run` deliberately do not — a fork inherits
/// whatever its snapshot carried, and `run` boxes are ephemeral.
pub(crate) fn with_herdr_vsock(
    builder: SandboxBuilder,
    socket_path: Option<std::path::PathBuf>,
) -> SandboxBuilder {
    match herdr_vsock_route(socket_path) {
        Some((path, port)) => builder.vsock(path, port),
        None => builder,
    }
}

pub(crate) fn herdr_socket_path_from_env() -> Option<std::path::PathBuf> {
    std::env::var_os("HERDR_SOCKET_PATH").map(std::path::PathBuf::from)
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

#[cfg(all(test, unix))]
mod herdr_vsock_tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn bound_socket() -> (std::path::PathBuf, UnixListener) {
        // Unique per call: tests run in parallel in one process, so a bare pid
        // path races (one test's remove_file lands between the other's remove
        // and bind -> AddrInUse flake).
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "lilbox-herdr-vsock-test-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        (path, listener)
    }

    #[test]
    fn unset_env_means_no_route() {
        assert_eq!(herdr_vsock_route(None), None);
    }

    #[test]
    fn missing_socket_means_no_route() {
        let path =
            std::env::temp_dir().join(format!("lilbox-herdr-vsock-absent-{}", std::process::id()));
        assert_eq!(herdr_vsock_route(Some(path)), None);
    }

    #[test]
    fn non_socket_path_means_no_route() {
        let path =
            std::env::temp_dir().join(format!("lilbox-herdr-vsock-file-{}", std::process::id()));
        std::fs::write(&path, b"not a socket").unwrap();
        assert_eq!(herdr_vsock_route(Some(path.clone())), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn live_socket_gets_route_on_fixed_port() {
        let (path, _listener) = bound_socket();
        let route = herdr_vsock_route(Some(path.clone())).unwrap();
        // Pin the literal: the guest-side shim (#130) hardcodes this port, so a
        // constant change without a guest change is a silent break.
        assert_eq!(route, (path.clone(), 47100));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn route_is_serialized_into_builder_config() {
        let (path, _listener) = bound_socket();
        let config = with_herdr_vsock(
            Sandbox::builder("vsock-route-test").image("python"),
            Some(path.clone()),
        )
        .build()
        .await
        .unwrap();
        let serialized = serde_json::to_string(&config).unwrap();

        assert!(serialized.contains(path.to_str().unwrap()));
        assert!(serialized.contains("\"port\":47100"));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn builder_passthrough_when_no_route() {
        // With no socket, with_herdr_vsock must add no route (and not error).
        let config = with_herdr_vsock(
            Sandbox::builder("vsock-passthrough-test").image("python"),
            None,
        )
        .build()
        .await
        .unwrap();
        let serialized = serde_json::to_string(&config).unwrap();
        // vsock serializes with skip_serializing_if = is_empty, so no route
        // means the key is absent entirely.
        assert!(!serialized.contains("\"vsock\""));
    }
}
