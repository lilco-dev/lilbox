//! `lilbox gateway --listen`: lilbox as its own SSH server (russh), the
//! exe.dev-style UX without a second OpenSSH instance.
//!
//! - `ssh <box>@host` — attach to a running box (username routing)
//! - `ssh new@host` — create a box and attach
//! - `ssh host <cmd>` — allowlisted management commands (same grammar and
//!   allowlist as the forced-command gateway; see [`authorize_argv`])
//!
//! Channels are bridged to `lilbox` subprocesses: on a pty when the client
//! requested one (so attach, resize, and Ctrl-C behave), on plain pipes
//! otherwise. Exit status follows the child. Auth is a flat
//! authorized_keys-format file — unlike OpenSSH, authz is now OUR code; keep
//! `authorize_argv`'s allowlist the only dispatch path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use russh::keys::{Algorithm, PrivateKey, PublicKey};
use russh::server::{Auth, ChannelOpenHandle, Msg, Server as _, Session};
use russh::{ChannelId, MethodSet, Pty};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::cli::Command;
use crate::commands::gateway::{GatewayReject, authorize_argv};

pub(crate) struct Options {
    pub(crate) listen: String,
    /// authorized_keys-format file of accepted client keys.
    pub(crate) keys: PathBuf,
}

struct State {
    /// Path to the running lilbox binary, re-exec'd per channel.
    exe: PathBuf,
    db_path: PathBuf,
    keys: Vec<PublicKey>,
}

pub(crate) async fn run(db_path: PathBuf, opts: Options) -> Result<i32> {
    let keys = load_authorized_keys(&opts.keys)?;
    let host_key_path = db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("gateway_host_ed25519");
    let host_key = host_key(&host_key_path)?;
    let exe = std::env::current_exe().context("resolve own exe")?;

    let mut methods = MethodSet::empty();
    methods.push(russh::MethodKind::PublicKey);
    let config = Arc::new(russh::server::Config {
        keys: vec![host_key],
        methods,
        auth_rejection_time: Duration::from_millis(500),
        auth_rejection_time_initial: Some(Duration::ZERO),
        // Sessions are box attaches; they can legitimately idle for hours.
        inactivity_timeout: None,
        ..Default::default()
    });
    let listener = TcpListener::bind(&opts.listen)
        .await
        .with_context(|| format!("bind {}", opts.listen))?;
    eprintln!("lilbox gateway listening on {}", listener.local_addr()?);
    eprintln!(
        "  {} authorized key(s) from {}",
        keys.len(),
        opts.keys.display()
    );

    let mut server = Server {
        state: Arc::new(State { exe, db_path, keys }),
    };
    server
        .run_on_socket(config, &listener)
        .await
        .context("serve")?;
    Ok(0)
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

/// Load (or create on first run) the server's ed25519 host key.
fn host_key(path: &Path) -> Result<PrivateKey> {
    if path.exists() {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read host key {}", path.display()))?;
        return PrivateKey::from_openssh(&text)
            .map_err(|e| anyhow::anyhow!("bad host key at {}: {e}", path.display()));
    }
    let key = PrivateKey::random(&mut rand10::rng(), Algorithm::Ed25519)
        .map_err(|e| anyhow::anyhow!("generate host key: {e}"))?;
    let openssh = key
        .to_openssh(russh::keys::ssh_key::LineEnding::LF)
        .map_err(|e| anyhow::anyhow!("encode host key: {e}"))?;
    // Host key is a secret: 0600 like OpenSSH's ssh_host_*_key files.
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts
        .open(path)
        .with_context(|| format!("create host key {}", path.display()))?;
    use std::io::Write as _;
    file.write_all(openssh.as_bytes())?;
    eprintln!("lilbox gateway: generated host key at {}", path.display());
    Ok(key)
}

/// Parse an authorized_keys-format file (plain `algo base64 [comment]` lines;
/// OpenSSH per-line options like `command=...` are not supported — the
/// gateway itself enforces the allowlist).
fn load_authorized_keys(path: &Path) -> Result<Vec<PublicKey>> {
    let text = std::fs::read_to_string(path).with_context(|| {
        format!(
            "gateway: no authorized keys file at {} — add client public keys (one per line) and restart",
            path.display()
        )
    })?;
    let mut keys = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match PublicKey::from_openssh(line) {
            Ok(key) => keys.push(key),
            Err(e) => eprintln!(
                "gateway: skipping unparsable key at {}:{}: {e}",
                path.display(),
                n + 1
            ),
        }
    }
    if keys.is_empty() {
        bail!(
            "gateway: {} contains no usable public keys; refusing to start an unreachable server",
            path.display()
        );
    }
    Ok(keys)
}

// ---------------------------------------------------------------------------
// Routing (pure, unit-tested)
// ---------------------------------------------------------------------------

/// Turn an exec request's command line into the child lilbox argv, applying
/// the gateway allowlist and direct-attach rewrite. With a pty, `new`
/// additionally attaches after boot (exe.dev's `ssh -t host new` UX).
fn route_exec(
    original: &str,
    has_box: &dyn Fn(&str) -> bool,
    has_pty: bool,
) -> std::result::Result<Vec<String>, String> {
    let argv = shlex::split(original)
        .ok_or_else(|| "could not parse command (unbalanced quotes)".to_string())?;
    match authorize_argv(argv, has_box) {
        Ok((command, mut argv)) => {
            if has_pty && matches!(command, Command::New(_)) {
                argv.push("--attach".to_string());
            }
            Ok(argv)
        }
        Err(GatewayReject::Empty) => Err("no command; try `ls` or `new`".into()),
        Err(GatewayReject::Unshlexable) => {
            Err("could not parse command (unbalanced quotes)".into())
        }
        Err(GatewayReject::Unparseable(error)) => Err(error.to_string()),
        Err(GatewayReject::NotAllowed(name)) => {
            Err(format!("'{name}' is not permitted over the gateway"))
        }
    }
}

/// A bare shell request routes on the username: an existing box name attaches,
/// `new` creates-and-attaches, anything else is an error.
fn route_shell(
    username: &str,
    has_box: &dyn Fn(&str) -> bool,
) -> std::result::Result<Vec<String>, String> {
    if username == "new" {
        return Ok(vec!["new".to_string(), "--attach".to_string()]);
    }
    if has_box(username) {
        return Ok(vec!["ssh".to_string(), username.to_string()]);
    }
    Err(format!(
        "no box named '{username}' — ssh <box>@<host> to attach, ssh new@<host> to create, ssh <host> ls to list"
    ))
}

/// Box existence check against the state DB. Opens a short-lived read
/// connection per call — cheap for a local sqlite file and sidesteps sharing
/// rusqlite's !Sync connection across session tasks.
fn box_exists(db_path: &Path, name: &str) -> bool {
    rusqlite::Connection::open(db_path)
        .ok()
        .and_then(|db| {
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM boxes WHERE name=?1)",
                [name],
                |row| row.get::<_, bool>(0),
            )
            .ok()
        })
        .unwrap_or(false)
}

fn box_checker(db_path: &Path) -> impl Fn(&str) -> bool {
    let db_path = db_path.to_path_buf();
    move |name| box_exists(&db_path, name)
}

// ---------------------------------------------------------------------------
// Server + session handler
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Server {
    state: Arc<State>,
}

impl russh::server::Server for Server {
    type Handler = Handler;

    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Handler {
        Handler {
            state: self.state.clone(),
            username: String::new(),
            channels: HashMap::new(),
        }
    }
}

/// Where client input goes. The pty write half doubles as the resize handle.
enum ChildIo {
    Pty(pty_process::OwnedWritePty),
    Pipe(tokio::process::ChildStdin),
}

struct ChannelState {
    term: String,
    /// Set by pty_request, taken when the child spawns.
    pts: Option<pty_process::Pts>,
    pty_read: Option<pty_process::OwnedReadPty>,
    io: Option<ChildIo>,
    /// Child pid, for teardown on channel close.
    pid: Option<u32>,
}

impl ChannelState {
    fn new() -> Self {
        Self {
            term: String::new(),
            pts: None,
            pty_read: None,
            io: None,
            pid: None,
        }
    }
}

struct Handler {
    state: Arc<State>,
    username: String,
    channels: HashMap<ChannelId, ChannelState>,
}

impl Handler {
    fn key_allowed(keys: &[PublicKey], key: &PublicKey) -> bool {
        // Compare key material only: PublicKey's Eq includes the comment, and
        // the wire key never carries the authorized_keys comment.
        keys.iter().any(|k| k.key_data() == key.key_data())
    }

    /// Reject a channel request with a message and a nonzero exit status.
    fn reject_channel(
        session: &mut Session,
        channel: ChannelId,
        message: String,
    ) -> Result<(), russh::Error> {
        session.data(channel, format!("gateway: {message}\r\n").into_bytes())?;
        session.exit_status_request(channel, 2)?;
        session.close(channel)?;
        Ok(())
    }

    async fn spawn_child(
        &mut self,
        channel: ChannelId,
        argv: Vec<String>,
        session: &mut Session,
    ) -> Result<(), russh::Error> {
        let Some(ch) = self.channels.get_mut(&channel) else {
            return Ok(());
        };
        let handle = session.handle();
        let spawned = if let Some(pts) = ch.pts.take() {
            let Some(read) = ch.pty_read.take() else {
                return Self::reject_channel(session, channel, "pty state lost".into());
            };
            spawn_pty(&self.state.exe, &argv, &ch.term, pts, read)
        } else {
            spawn_pipes(&self.state.exe, &argv)
        };
        match spawned {
            Ok(mut spawned) => {
                ch.pid = spawned.pid;
                if let Some(stdin) = spawned.stdin.take() {
                    ch.io = Some(ChildIo::Pipe(stdin));
                }
                tokio::spawn(pump(channel, handle, spawned));
            }
            Err(error) => {
                Self::reject_channel(session, channel, format!("spawn failed: {error:#}"))?;
            }
        }
        Ok(())
    }
}

struct Spawned {
    pid: Option<u32>,
    /// Present only for pipe-spawned children; pty stdin is already in the
    /// channel's `ChildIo::Pty` write half.
    stdin: Option<tokio::process::ChildStdin>,
    reader: Pin<Box<dyn AsyncRead + Send>>,
    stderr: Option<tokio::process::ChildStderr>,
    child: tokio::process::Child,
}

fn spawn_pty(
    exe: &Path,
    argv: &[String],
    term: &str,
    pts: pty_process::Pts,
    read: pty_process::OwnedReadPty,
) -> Result<Spawned> {
    let mut command = pty_process::Command::new(exe);
    command = command.args(argv);
    if !term.is_empty() {
        command = command.env("TERM", term);
    }
    let child = command.spawn(pts).context("spawn on pty")?;
    let pid = child.id();
    Ok(Spawned {
        pid,
        stdin: None,
        reader: Box::pin(read),
        stderr: None,
        child,
    })
}

fn spawn_pipes(exe: &Path, argv: &[String]) -> Result<Spawned> {
    let mut child = tokio::process::Command::new(exe)
        .args(argv)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("spawn")?;
    let pid = child.id();
    let stdin = child.stdin.take().context("child stdin")?;
    let stdout = child.stdout.take().context("child stdout")?;
    let stderr = child.stderr.take();
    Ok(Spawned {
        pid,
        stdin: Some(stdin),
        reader: Box::pin(stdout),
        stderr,
        child,
    })
}

/// Pump child output to the channel, then report the exit status and close.
async fn pump(channel: ChannelId, handle: russh::server::Handle, spawned: Spawned) {
    use tokio::io::AsyncReadExt as _;
    let Spawned {
        mut reader,
        stderr,
        mut child,
        ..
    } = spawned;
    let stderr_task = stderr.map(|mut err| {
        let handle = handle.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 16384];
            loop {
                match err.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if handle
                            .extended_data(channel, 1, buf[..n].to_vec())
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        })
    });

    let mut buf = vec![0u8; 16384];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if handle.data(channel, buf[..n].to_vec()).await.is_err() {
                    break;
                }
            }
        }
    }
    if let Some(task) = stderr_task {
        let _ = task.await;
    }
    let code = child
        .wait()
        .await
        .ok()
        .and_then(|status| status.code())
        .unwrap_or(1);
    let _ = handle.exit_status_request(channel, code as u32).await;
    let _ = handle.close(channel).await;
}

impl russh::server::Handler for Handler {
    type Error = russh::Error;

    async fn auth_publickey_offered(
        &mut self,
        _user: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        // Probe only: accept if the key is listed so the client proceeds to
        // sign; the real decision is in auth_publickey.
        if Self::key_allowed(&self.state.keys, public_key) {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        // Signature already verified by russh at this point.
        if Self::key_allowed(&self.state.keys, public_key) {
            self.username = user.to_string();
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: russh::Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), ChannelState::new());
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(Pty, u32)],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let (pty, pts) = pty_process::open().map_err(|_| russh::Error::Disconnect)?;
        pty.resize(pty_process::Size::new(row_height as u16, col_width as u16))
            .map_err(|_| russh::Error::Disconnect)?;
        let (read, write) = pty.into_split();
        if let Some(ch) = self.channels.get_mut(&channel) {
            ch.term = term.to_string();
            ch.pts = Some(pts);
            ch.pty_read = Some(read);
            ch.io = Some(ChildIo::Pty(write));
        }
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(ch) = self.channels.get_mut(&channel)
            && let Some(ChildIo::Pty(write)) = &ch.io
        {
            let _ = write.resize(pty_process::Size::new(row_height as u16, col_width as u16));
        }
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let route = route_shell(&self.username, &box_checker(&self.state.db_path));
        match route {
            Ok(argv) => self.spawn_child(channel, argv, session).await,
            Err(message) => Self::reject_channel(session, channel, message),
        }
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let original = String::from_utf8_lossy(data).to_string();
        let has_pty = self
            .channels
            .get(&channel)
            .is_some_and(|ch| ch.pts.is_some());
        let route = route_exec(&original, &box_checker(&self.state.db_path), has_pty);
        match route {
            Ok(argv) => self.spawn_child(channel, argv, session).await,
            Err(message) => Self::reject_channel(session, channel, message),
        }
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(io) = self
            .channels
            .get_mut(&channel)
            .and_then(|ch| ch.io.as_mut())
        {
            let stdin: &mut (dyn AsyncWrite + Unpin + Send) = match io {
                ChildIo::Pty(write) => write,
                ChildIo::Pipe(stdin) => stdin,
            };
            let _ = stdin.write_all(data).await;
            let _ = stdin.flush().await;
        }
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(ch) = self.channels.remove(&channel)
            && let Some(pid) = ch.pid
        {
            // Client went away mid-command: stop the child (the box itself
            // is detached and keeps running).
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_routes_box_name_to_attach() {
        let argv = route_shell("web", &|name| name == "web").unwrap();
        assert_eq!(argv, ["ssh", "web"]);
    }

    #[test]
    fn shell_routes_new_to_create_and_attach() {
        let argv = route_shell("new", &|_| false).unwrap();
        assert_eq!(argv, ["new", "--attach"]);
    }

    #[test]
    fn shell_rejects_unknown_box() {
        let err = route_shell("nope", &|_| false).unwrap_err();
        assert!(err.contains("no box named 'nope'"), "{err}");
    }

    #[test]
    fn exec_allowlists_and_rewrites_direct_attach() {
        let argv = route_exec("web -- uname -a", &|name| name == "web", false).unwrap();
        assert_eq!(argv, ["ssh", "web", "--", "uname", "-a"]);
    }

    #[test]
    fn exec_new_with_pty_attaches() {
        let argv = route_exec("new --image node my-box", &|_| false, true).unwrap();
        assert_eq!(argv, ["new", "--image", "node", "my-box", "--attach"]);
    }

    #[test]
    fn exec_new_without_pty_stays_detached() {
        let argv = route_exec("new my-box", &|_| false, false).unwrap();
        assert_eq!(argv, ["new", "my-box"]);
    }

    #[test]
    fn exec_rejects_disallowed_commands() {
        let err = route_exec("exec web -- ls", &|_| true, false).unwrap_err();
        assert!(err.contains("not permitted"), "{err}");
    }
}
