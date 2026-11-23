use std::path::PathBuf;
use std::{env, fs, process::Command as ProcessCommand};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Local;
use microsandbox::Sandbox;
use microsandbox::sandbox::fs::FsSetAttrs;
use rusqlite::params;
use serde_json::Value;

use crate::app::App;
use crate::cli::AgentArgs;
use crate::sandbox::{run_guest, stop_and_remove, with_secret_env};
use crate::util::{DEFAULT_GUEST_PORT, alloc_host_port, find_program, or_cleanup, random_name};

const AGENT_WORKDIR: &str = "/workspace";

/// How an agent box authenticates to Anthropic.
#[derive(Debug, PartialEq, Eq)]
enum AuthMode {
    InheritLogin,
    InjectApiKey,
    WarnNoAuth,
}

/// Keep only the auth-relevant keys from a host ~/.claude.json blob.
fn claude_auth_slice(raw: &str) -> Result<Value> {
    let parsed: Value = serde_json::from_str(raw)?;
    let object = parsed
        .as_object()
        .ok_or_else(|| anyhow!("expected a JSON object"))?;
    let mut slice = serde_json::Map::new();
    for key in [
        "oauthAccount",
        "userID",
        "hasCompletedOnboarding",
        "lastOnboardingVersion",
    ] {
        if let Some(value) = object.get(key) {
            slice.insert(key.to_owned(), value.clone());
        }
    }
    Ok(Value::Object(slice))
}

/// Decide how an agent box authenticates to Anthropic.
fn decide_auth_mode(creds_exist: bool, no_claude_config: bool, api_key_set: bool) -> AuthMode {
    if creds_exist && !no_claude_config {
        AuthMode::InheritLogin
    } else if api_key_set {
        AuthMode::InjectApiKey
    } else {
        AuthMode::WarnNoAuth
    }
}

/// Resolve (host ~/.claude dir, host ~/.claude.json path) from an optional home dir.
fn host_claude_paths(home: Option<PathBuf>) -> Result<(PathBuf, PathBuf)> {
    let home = home.ok_or_else(|| anyhow!("could not determine home directory"))?;
    Ok((home.join(".claude"), home.join(".claude.json")))
}

/// Why post-`create_detached` provisioning is considered failed. `None` = success.
#[derive(Debug, PartialEq, Eq)]
enum ProvisionFailure {
    InstallFailed,
    ClaudeAbsent,
}

/// Classify the install shell's exit and the follow-up `command -v claude` probe.
/// An install that exits non-zero is InstallFailed regardless of the probe (its
/// symlink step is `||`-guarded, so exit 0 doesn't prove claude is on PATH — and
/// a non-zero exit means the install itself broke).
fn classify_install_probe(install_ok: bool, probe_ok: bool) -> Option<ProvisionFailure> {
    if !install_ok {
        Some(ProvisionFailure::InstallFailed)
    } else if !probe_ok {
        Some(ProvisionFailure::ClaudeAbsent)
    } else {
        None
    }
}

/// Best-effort chmod inside the box. Tightening perms on inherited credentials
/// is defense-in-depth on a single-tenant root VM, so a failure warns rather
/// than aborting a box that is otherwise provisioned and logged in.
async fn chmod_guest(sandbox: &Sandbox, path: &str, mode: u32) {
    let attrs = FsSetAttrs {
        mode: Some(mode),
        ..Default::default()
    };
    if let Err(err) = sandbox.fs().set_stat(path, true, attrs).await {
        eprintln!("warning: could not set mode {mode:o} on {path} in agent box: {err}");
    }
}

/// Parse repeatable `--env KEY=VAL` pairs into exec env for the guest.
/// `split_once` keeps any `=` in the value intact.
fn parse_exec_env(pairs: &[String]) -> Result<Vec<(String, String)>> {
    pairs
        .iter()
        .map(|pair| {
            let (key, value) = pair
                .split_once('=')
                .ok_or_else(|| anyhow!("--env expects KEY=VAL, got '{pair}'"))?;
            // Fail fast on anything that isn't a valid shell identifier — a
            // bad key would fail opaquely in the guest otherwise.
            let valid = !key.is_empty()
                && key.chars().enumerate().all(|(i, c)| {
                    c == '_'
                        || c.is_ascii_uppercase()
                        || c.is_ascii_lowercase()
                        || (i > 0 && c.is_ascii_digit())
                });
            if !valid {
                bail!("--env key '{key}' is not a valid name ([A-Za-z_][A-Za-z0-9_]*)");
            }
            Ok((key.to_owned(), value.to_owned()))
        })
        .collect()
}

/// Build the guest command that runs the agent task.
fn agent_launch_command(task: &str) -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        "IS_SANDBOX=1 exec claude -p \"$1\" --dangerously-skip-permissions".into(),
        "sh".into(),
        task.into(),
    ]
}

pub(crate) async fn cmd_agent(app: &App, args: AgentArgs) -> Result<i32> {
    let exec_env = parse_exec_env(&args.env)?;
    let name = match args.name {
        Some(name) => name,
        None => random_name(app)?,
    };
    if app.row(&name)?.is_some() {
        bail!("box '{name}' already exists");
    }
    let workspace = if let Some(url) = args.clone {
        let repo = url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("repo")
            .trim_end_matches(".git");
        let dest = app.workspaces_dir().join(repo);
        if dest.exists() {
            bail!("clone target already exists: {}", dest.display());
        }
        fs::create_dir_all(dest.parent().unwrap())?;
        let git = find_program("git").ok_or_else(|| anyhow!("git not found"))?;
        let status = ProcessCommand::new(git)
            .args(["clone", "--depth", "1", &url])
            .arg(&dest)
            .status()?;
        if !status.success() {
            bail!("git clone failed");
        }
        dest
    } else {
        args.workspace
            .unwrap_or(env::current_dir()?)
            .canonicalize()?
    };
    if !workspace.is_dir() {
        bail!("workspace not found: {}", workspace.display());
    }
    let host_port = alloc_host_port()?;
    let mut builder = Sandbox::builder(&name)
        .image(args.image.as_str())
        .port(host_port, DEFAULT_GUEST_PORT)
        .workdir(AGENT_WORKDIR)
        .volume(AGENT_WORKDIR, |m| m.bind(&workspace))
        .detached(true)
        .label("dev.lilbox.managed", "true")
        .label("dev.lilbox.kind", "agent");
    let claude_paths = host_claude_paths(dirs::home_dir()).ok();
    let creds_src = claude_paths
        .as_ref()
        .map(|(dir, _)| dir.join(".credentials.json"));
    let creds_exist = creds_src.as_ref().is_some_and(|p| p.exists());
    let api_key_set = env::var(&args.key_env).is_ok_and(|value| !value.is_empty());
    let mode = decide_auth_mode(creds_exist, args.no_claude_config, api_key_set);
    match mode {
        AuthMode::InjectApiKey => {
            builder = with_secret_env(builder, &args.key_env, &args.key_host);
        }
        AuthMode::WarnNoAuth => {
            eprintln!(
                "warning: no Claude login inherited and {} is not set; the agent cannot authenticate",
                args.key_env
            );
        }
        AuthMode::InheritLogin => {
            println!(
                "inheriting host Claude login from {}",
                creds_src.as_ref().unwrap().display()
            );
        }
    }
    println!(
        "booting agent box {name} ({}, workspace {}) ...",
        args.image,
        workspace.display()
    );
    // Register termination handlers BEFORE the VM exists, so a (rare) signal
    // registration failure aborts here rather than orphaning a box. Mirrors
    // `commands::new`: when `lilbox agent` runs over SSH, a client disconnect
    // arrives as SIGHUP/SIGTERM (not SIGINT) — cover all three, or the atomic
    // teardown below wouldn't fire for the case that most needs it.
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sighup = signal(SignalKind::hangup())?;
    let sandbox = builder.create_detached().await?;
    let provision = async {
        if let Some(source) = &args.agents_file {
            let target = workspace.join("AGENTS.md");
            if source.canonicalize()? != target.canonicalize().unwrap_or_else(|_| target.clone()) {
                fs::copy(source, target)?;
            }
        }
        let install = "command -v claude >/dev/null 2>&1 || { command -v curl >/dev/null 2>&1 || ((apt-get update -qq && apt-get install -y -qq curl) 2>/dev/null || apk add --no-cache curl); curl -fsSL https://claude.ai/install.sh | bash; [ ! -x \"$HOME/.local/bin/claude\" ] || ln -sf \"$HOME/.local/bin/claude\" /usr/local/bin/claude; }";
        let output = sandbox.shell(install).await?;
        let install_ok = output.status().success;
        let probe = sandbox.shell("command -v claude >/dev/null 2>&1").await?;
        let probe_ok = probe.status().success;
        match classify_install_probe(install_ok, probe_ok) {
            Some(ProvisionFailure::InstallFailed) => {
                bail!(
                    "claude install failed: {}",
                    output.stderr().unwrap_or_default()
                );
            }
            Some(ProvisionFailure::ClaudeAbsent) => {
                bail!("claude is not on PATH in the box after install");
            }
            None => {}
        }
        if mode == AuthMode::InheritLogin {
            let (claude_dir, claude_json) = claude_paths.as_ref().unwrap();
            let credentials_path = creds_src.as_ref().unwrap();
            let guest_claude_dir = "/root/.claude";
            let guest_credentials = "/root/.claude/.credentials.json";
            if let Err(err) = sandbox.fs().mkdir(guest_claude_dir).await
                && !sandbox.fs().exists(guest_claude_dir).await.unwrap_or(false)
            {
                bail!("could not create {guest_claude_dir} in agent box: {err}");
            }
            sandbox
                .fs()
                .copy_from_host(credentials_path, guest_credentials)
                .await
                .with_context(|| {
                    format!(
                        "could not copy {} into agent box",
                        credentials_path.display()
                    )
                })?;
            chmod_guest(&sandbox, guest_credentials, 0o600).await;
            chmod_guest(&sandbox, guest_claude_dir, 0o700).await;
            let settings_path = claude_dir.join("settings.json");
            if settings_path.exists() {
                sandbox
                    .fs()
                    .copy_from_host(&settings_path, "/root/.claude/settings.json")
                    .await
                    .with_context(|| {
                        format!("could not copy {} into agent box", settings_path.display())
                    })?;
            }
            let slice = fs::read_to_string(claude_json)
                .map_err(anyhow::Error::from)
                .and_then(|raw| claude_auth_slice(&raw));
            match slice {
                Ok(slice) => {
                    let serialized = serde_json::to_vec(&slice)?;
                    sandbox
                        .fs()
                        .write("/root/.claude.json", serialized)
                        .await
                        .with_context(|| "could not write /root/.claude.json into agent box")?;
                    chmod_guest(&sandbox, "/root/.claude.json", 0o600).await;
                }
                Err(_) => {
                    eprintln!(
                        "warning: account identity/onboarding not inherited (could not read or parse {})",
                        claude_json.display()
                    );
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    };
    tokio::pin!(provision);
    let outcome = tokio::select! {
        r = &mut provision => r,
        _ = tokio::signal::ctrl_c() => Err(anyhow!("interrupted before the box finished provisioning")),
        _ = sigterm.recv() => Err(anyhow!("terminated before the box finished provisioning")),
        _ = sighup.recv() => Err(anyhow!("disconnected before the box finished provisioning")),
    };
    or_cleanup(outcome, || stop_and_remove(&name)).await?;
    // Commit point. Guard the row write too: if it fails (e.g. SQLITE_BUSY, or a
    // duplicate name that appeared in the widened window since the existence
    // check), the VM is already built, so tear it down rather than orphan it.
    let insert = app.db.execute("INSERT INTO boxes(name,image,guest_port,host_port,created,comment) VALUES(?1,?2,?3,?4,?5,'agent')",
        params![name, args.image, DEFAULT_GUEST_PORT, host_port, Local::now().format("%Y-%m-%d %H:%M:%S").to_string()])
        .map(|_| ())
        .map_err(anyhow::Error::from);
    or_cleanup(insert, || stop_and_remove(&name)).await?;
    println!("agent box {name} ready (workspace mounted at {AGENT_WORKDIR})");
    if args.task.is_empty() {
        return Ok(0);
    }
    let task = args.task.join(" ");
    let command = agent_launch_command(&task);
    run_guest(&sandbox, &command, Some(AGENT_WORKDIR), &exec_env).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_command_execs_claude_with_the_task() {
        let cmd = agent_launch_command("fix the bug");
        assert_eq!(
            cmd,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "IS_SANDBOX=1 exec claude -p \"$1\" --dangerously-skip-permissions".to_string(),
                "sh".to_string(),
                "fix the bug".to_string(),
            ]
        );
    }

    #[test]
    fn parse_exec_env_accepts_pairs_and_preserves_value_equals() {
        let env = parse_exec_env(&["MY_TOKEN=a=b=c".to_string(), "OTHER=1".to_string()]).unwrap();
        assert_eq!(
            env,
            vec![
                ("MY_TOKEN".to_string(), "a=b=c".to_string()),
                ("OTHER".to_string(), "1".to_string()),
            ]
        );
    }

    #[test]
    fn parse_exec_env_rejects_malformed_pairs() {
        assert!(parse_exec_env(&["NOEQUALS".to_string()]).is_err());
        assert!(parse_exec_env(&["=value".to_string()]).is_err());
    }

    #[test]
    fn parse_exec_env_rejects_invalid_key_names() {
        assert!(parse_exec_env(&["1ST=x".to_string()]).is_err());
        assert!(parse_exec_env(&["MY-KEY=x".to_string()]).is_err());
        assert!(parse_exec_env(&["MY KEY=x".to_string()]).is_err());
        assert!(parse_exec_env(&["_OK_9=x".to_string()]).is_ok());
    }

    #[tokio::test]
    async fn agent_secret_is_stored_as_an_environment_reference() {
        let config = with_secret_env(
            Sandbox::builder("agent-secret-test").image("python"),
            "ANTHROPIC_API_KEY",
            "api.anthropic.com",
        )
        .build()
        .await
        .unwrap();
        let serialized = serde_json::to_string(&config).unwrap();

        assert!(serialized.contains("ANTHROPIC_API_KEY"));
        assert!(serialized.contains("\"kind\":\"env\""));
        assert!(!serialized.contains("sk-ant-test-secret"));
    }

    #[test]
    fn claude_auth_slice_keeps_only_auth_keys_and_drops_extras() {
        let raw = serde_json::json!({
            "oauthAccount": {"email": "a@example.com"},
            "userID": "user-123",
            "hasCompletedOnboarding": true,
            "lastOnboardingVersion": "1.2.3",
            "someUnrelatedKey": "drop me",
            "anotherExtra": 42,
        })
        .to_string();

        let expected = serde_json::json!({
            "oauthAccount": {"email": "a@example.com"},
            "userID": "user-123",
            "hasCompletedOnboarding": true,
            "lastOnboardingVersion": "1.2.3",
        });

        let actual = claude_auth_slice(&raw).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn claude_auth_slice_omits_missing_keys_entirely() {
        let raw = serde_json::json!({
            "userID": "user-123",
            "hasCompletedOnboarding": false,
            "unrelated": "nope",
        })
        .to_string();

        let actual = claude_auth_slice(&raw).unwrap();
        let obj = actual.as_object().unwrap();

        assert!(!obj.contains_key("oauthAccount"));
        assert!(!obj.contains_key("lastOnboardingVersion"));
        assert_eq!(obj.get("userID").unwrap(), "user-123");
        assert_eq!(obj.get("hasCompletedOnboarding").unwrap(), false);
    }

    #[test]
    fn claude_auth_slice_is_empty_object_when_none_present() {
        let raw = serde_json::json!({
            "unrelatedA": 1,
            "unrelatedB": "x",
        })
        .to_string();

        let actual = claude_auth_slice(&raw).unwrap();
        assert_eq!(actual, serde_json::json!({}));
    }

    #[test]
    fn claude_auth_slice_preserves_nested_oauth_account_verbatim() {
        let raw = serde_json::json!({
            "oauthAccount": {
                "email": "a@example.com",
                "nested": {"deeper": [1, 2, 3]},
            },
        })
        .to_string();

        let actual = claude_auth_slice(&raw).unwrap();
        assert_eq!(
            actual.get("oauthAccount").unwrap(),
            &serde_json::json!({
                "email": "a@example.com",
                "nested": {"deeper": [1, 2, 3]},
            })
        );
    }

    #[test]
    fn claude_auth_slice_rejects_empty_string() {
        assert!(claude_auth_slice("").is_err());
    }

    #[test]
    fn claude_auth_slice_rejects_malformed_json() {
        assert!(claude_auth_slice("{ not json").is_err());
    }

    #[test]
    fn claude_auth_slice_rejects_non_object_array() {
        assert!(claude_auth_slice("[]").is_err());
    }

    #[test]
    fn claude_auth_slice_rejects_non_object_number() {
        assert!(claude_auth_slice("42").is_err());
    }

    #[test]
    fn decide_auth_mode_inherits_login_when_creds_exist_and_config_present_with_api_key() {
        // Invariant: inherited login must never also inject the key.
        assert_eq!(decide_auth_mode(true, false, true), AuthMode::InheritLogin);
    }

    #[test]
    fn decide_auth_mode_inherits_login_when_creds_exist_and_config_present_without_api_key() {
        // Invariant: inherited login must never also inject the key.
        assert_eq!(decide_auth_mode(true, false, false), AuthMode::InheritLogin);
    }

    #[test]
    fn decide_auth_mode_injects_api_key_when_creds_exist_but_no_config_and_key_set() {
        assert_eq!(decide_auth_mode(true, true, true), AuthMode::InjectApiKey);
    }

    #[test]
    fn decide_auth_mode_warns_when_creds_exist_but_no_config_and_no_key() {
        assert_eq!(decide_auth_mode(true, true, false), AuthMode::WarnNoAuth);
    }

    #[test]
    fn decide_auth_mode_injects_api_key_when_no_creds_config_present_and_key_set() {
        assert_eq!(decide_auth_mode(false, false, true), AuthMode::InjectApiKey);
    }

    #[test]
    fn decide_auth_mode_warns_when_no_creds_config_present_and_no_key() {
        assert_eq!(decide_auth_mode(false, false, false), AuthMode::WarnNoAuth);
    }

    #[test]
    fn decide_auth_mode_injects_api_key_when_no_creds_no_config_and_key_set() {
        assert_eq!(decide_auth_mode(false, true, true), AuthMode::InjectApiKey);
    }

    #[test]
    fn decide_auth_mode_warns_when_no_creds_no_config_and_no_key() {
        assert_eq!(decide_auth_mode(false, true, false), AuthMode::WarnNoAuth);
    }

    #[test]
    fn host_claude_paths_resolves_dir_and_json_from_given_home() {
        let (dir, json) = host_claude_paths(Some(PathBuf::from("/tmp/x"))).unwrap();
        assert_eq!(dir, PathBuf::from("/tmp/x/.claude"));
        assert_eq!(json, PathBuf::from("/tmp/x/.claude.json"));
    }

    #[test]
    fn host_claude_paths_errs_when_home_is_none() {
        assert!(host_claude_paths(None).is_err());
    }

    #[test]
    fn classify_install_probe_none_when_install_and_probe_ok() {
        assert_eq!(classify_install_probe(true, true), None);
    }

    #[test]
    fn classify_install_probe_claude_absent_when_install_ok_but_probe_fails() {
        assert_eq!(
            classify_install_probe(true, false),
            Some(ProvisionFailure::ClaudeAbsent)
        );
    }

    #[test]
    fn classify_install_probe_install_failed_when_install_fails_and_probe_ok() {
        assert_eq!(
            classify_install_probe(false, true),
            Some(ProvisionFailure::InstallFailed)
        );
    }

    #[test]
    fn classify_install_probe_install_failed_when_both_fail() {
        assert_eq!(
            classify_install_probe(false, false),
            Some(ProvisionFailure::InstallFailed)
        );
    }
}
