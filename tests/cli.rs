use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn temp_home(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("lilbox-{label}-{}-{nonce}", std::process::id()))
}

/// Build a `lilbox` invocation pinned to `home`, with the XDG_* overrides
/// cleared so `dirs` resolves state strictly under `home` (an inherited
/// `XDG_CONFIG_HOME` on the runner would otherwise send state outside it,
/// escaping the no-state-init guards below).
fn lilbox_cmd(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lilbox"));
    cmd.env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_DATA_HOME");
    cmd
}

/// Assert `lilbox` initialized no on-disk state under `home`. `App::new()`
/// writes to the XDG dirs — `~/.config/lilbox`, `~/.local/state/lilbox`,
/// `~/.local/share/lilbox` — not the legacy `~/.lilbox`, so checking those is
/// what actually proves a command short-circuited before app init.
fn assert_no_lilbox_state(home: &Path) {
    for rel in [
        ".config/lilbox",
        ".local/state/lilbox",
        ".local/share/lilbox",
        ".lilbox",
    ] {
        assert!(
            !home.join(rel).exists(),
            "lilbox initialized state at {rel}"
        );
    }
}

#[test]
fn help_does_not_initialize_user_state() {
    let home = temp_home("help");
    let output = Command::new(env!("CARGO_BIN_EXE_lilbox"))
        .arg("--help")
        .env("HOME", &home)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(!home.join(".lilbox").exists());
    let _ = fs::remove_dir_all(home);
}

#[test]
fn parse_errors_do_not_initialize_user_state() {
    let home = temp_home("parse-error");
    let output = Command::new(env!("CARGO_BIN_EXE_lilbox"))
        .arg("not-a-command")
        .env("HOME", &home)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(!home.join(".lilbox").exists());
    let _ = fs::remove_dir_all(home);
}

#[test]
fn completions_prints_script_per_shell() {
    // Markers are clap_complete v4's per-shell template idioms — if a future
    // clap_complete bump changes its generated boilerplate, these are the
    // strings to re-check.
    let cases = [
        ("bash", "complete"),
        ("zsh", "#compdef"),
        ("fish", "complete -c lilbox"),
        ("elvish", "edit:completion"),
        ("powershell", "Register-ArgumentCompleter"),
    ];

    for (shell, marker) in cases {
        let home = temp_home(&format!("completions-{shell}"));
        let output = lilbox_cmd(&home)
            .args(["completions", shell])
            .output()
            .unwrap();

        assert!(output.status.success(), "shell {shell} did not succeed");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(!stdout.is_empty(), "shell {shell} produced empty stdout");
        assert!(
            stdout.contains("lilbox"),
            "shell {shell} stdout missing binary name: {stdout}"
        );
        assert!(
            stdout.contains(marker),
            "shell {shell} stdout missing marker {marker:?}: {stdout}"
        );
        let _ = fs::remove_dir_all(home);
    }
}

#[test]
fn completions_does_not_initialize_user_state() {
    let home = temp_home("completions-state");
    let output = lilbox_cmd(&home)
        .args(["completions", "bash"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_no_lilbox_state(&home);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn completions_rejects_invalid_shell() {
    let home = temp_home("completions-invalid-shell");
    let output = lilbox_cmd(&home)
        .args(["completions", "notashell"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_no_lilbox_state(&home);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn completions_requires_shell_argument() {
    let home = temp_home("completions-missing-shell");
    let output = lilbox_cmd(&home).arg("completions").output().unwrap();

    assert!(!output.status.success());
    assert_no_lilbox_state(&home);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn join_and_leave_are_discoverable_in_help() {
    let home = temp_home("join-leave-help");
    let output = lilbox_cmd(&home).arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("join"), "help missing join: {stdout}");
    assert!(stdout.contains("leave"), "help missing leave: {stdout}");
    let _ = fs::remove_dir_all(home);
}

#[test]
fn fork_help_documents_force() {
    let home = temp_home("fork-help");
    let output = lilbox_cmd(&home).args(["fork", "--help"]).output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--force"),
        "fork help missing --force: {stdout}"
    );
    let _ = fs::remove_dir_all(home);
}

/// `join`, `leave`, and `fork` all resolve the box row before touching a
/// sandbox, so an unknown box is a clean error with no KVM involved. This also
/// proves the new subcommands are wired through `dispatch`.
#[test]
fn join_leave_and_fork_reject_unknown_boxes() {
    for args in [["join", "ghost"], ["leave", "ghost"], ["fork", "ghost"]] {
        let home = temp_home(&format!("unknown-{}", args[0]));
        let output = lilbox_cmd(&home).args(args).output().unwrap();

        assert!(
            !output.status.success(),
            "`lilbox {} ghost` unexpectedly succeeded",
            args[0]
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("no box named 'ghost'"),
            "`lilbox {} ghost` stderr missing the unknown-box error: {stderr}",
            args[0]
        );
        let _ = fs::remove_dir_all(home);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn gateway_attaches_to_a_tailnet_box_with_a_real_proxy_shell() {
    use std::os::unix::fs::PermissionsExt;

    let home = temp_home("gateway-tailnet");
    let state = home.join("state");
    let cmd = || {
        let mut cmd = lilbox_cmd(&home);
        cmd.env("XDG_STATE_HOME", &state);
        cmd
    };
    let init = cmd()
        .env_remove("SSH_ORIGINAL_COMMAND")
        .arg("gateway")
        .output()
        .unwrap();
    assert!(init.status.success(), "{:?}", init);
    let db = rusqlite::Connection::open(state.join("lilbox/state.db")).unwrap();
    db.execute(
        "INSERT INTO boxes(name,image,created,tailscale_node) VALUES (?1,?2,?3,?4)",
        ["my-box", "debian", "2024-01-01", "my-box.example.ts.net"],
    )
    .unwrap();

    let tailscale = home.join("tailscale");
    fs::write(
        &tailscale,
        "#!/bin/sh\n\
         test \"$SHELL\" = /bin/sh && test \"$#\" = 2 && \
         test \"$1\" = ssh && test \"$2\" = root@my-box.example.ts.net\n",
    )
    .unwrap();
    fs::set_permissions(&tailscale, fs::Permissions::from_mode(0o755)).unwrap();
    let attach = cmd()
        .env("PATH", &home)
        .env("SHELL", "/usr/local/bin/lilbox-gateway-shell")
        .env("SSH_ORIGINAL_COMMAND", "my-box")
        .arg("gateway")
        .output()
        .unwrap();
    fs::remove_dir_all(&home).unwrap();
    assert!(attach.status.success(), "{:?}", attach);
}

#[test]
fn completions_is_discoverable_in_help() {
    let home = temp_home("completions-help");
    let output = lilbox_cmd(&home).arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("completions"));
    let _ = fs::remove_dir_all(home);
}
