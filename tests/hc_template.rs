use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_home(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("lilbox-{label}-{}-{nonce}", std::process::id()))
}

/// `lilbox hc install` writes outside lilbox's own XDG dirs (into
/// `herdr-core/templates`), so it must NOT go through `App::new` — a command
/// that short-circuits before app init leaves no state behind. Pin HOME and
/// clear XDG overrides like the other CLI tests.
fn lilbox_cmd(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lilbox"));
    cmd.env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_DATA_HOME");
    cmd
}

#[test]
fn hc_install_writes_script_and_template() {
    let home = temp_home("hc-install");
    let status = lilbox_cmd(&home).args(["hc", "install"]).status().unwrap();
    assert!(status.success());

    let script = home.join(".local/share/lilbox/hc/lilbox-hc-agent");
    let template = home.join(".config/herdr-core/templates/lilbox-agent.md");
    assert!(script.is_file(), "script missing at {}", script.display());
    assert!(
        template.is_file(),
        "template missing at {}",
        template.display()
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(fs::metadata(&script).unwrap().permissions().mode() & 0o111 != 0);
    }

    let body = fs::read_to_string(&template).unwrap();
    assert!(
        body.contains(&format!("command: {}", script.display())),
        "template must point command: at the installed script, got:\n{body}"
    );

    // The installed script is the binary's embedded copy; pin it to the repo's
    // review copy so an edit to one without the other fails loudly.
    let repo_script = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/contrib/hc/bin/lilbox-hc-agent"
    ))
    .unwrap();
    assert_eq!(fs::read_to_string(&script).unwrap(), repo_script);

    let _ = fs::remove_dir_all(home);
}

/// Stub `lilbox` that records its argv (NUL-separated) and answers `ls --json`
/// with no boxes, so the entrypoint takes the create-and-attach path.
fn stub_lilbox(bin_dir: &Path, log: &Path) {
    let stub = format!(
        "#!/bin/sh\nprintf '%s\\0' \"$@\" >>{}\n[ \"$1\" = ls ] && printf '[]'\nexit 0\n",
        log.display()
    );
    fs::write(bin_dir.join("lilbox"), stub).unwrap();
    make_executable(&bin_dir.join("lilbox"));
}

fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path).unwrap().permissions().mode() | 0o111;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
}

#[test]
fn agent_entrypoint_creates_box_then_attaches_agent() {
    if Command::new("jq").arg("--version").output().is_err() {
        eprintln!("skipping: jq is not installed");
        return;
    }
    let home = temp_home("hc-agent");
    lilbox_cmd(&home).args(["hc", "install"]).status().unwrap();

    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let log = home.join("lilbox.argv");
    stub_lilbox(&bin_dir, &log);

    let work = home.join("work/My_Repo.worktree");
    fs::create_dir_all(&work).unwrap();
    let name = {
        // hd-<slug>-<6 hex of sha256(canonical path)>, per the entrypoint:
        // the hash is over the path STRING, not the directory's contents.
        let out = Command::new("sh")
            .args([
                "-c",
                "realpath -m -- \"$1\" | tr -d '\\n' | sha256sum",
                "sh",
            ])
            .arg(&work)
            .output()
            .unwrap();
        let hex: String = String::from_utf8(out.stdout)
            .unwrap()
            .chars()
            .take(6)
            .collect();
        format!("hd-my-repo-worktree-{hex}")
    };

    let path_with_stub = format!("{}:{}", bin_dir.display(), std::env::var("PATH").unwrap());
    let status = Command::new(home.join(".local/share/lilbox/hc/lilbox-hc-agent"))
        .current_dir(&work)
        .env("PATH", &path_with_stub)
        .env("HOME", &home)
        .env("LILBOX_AGENT_CMD", "pi")
        .env_remove("LILBOX_IMAGE")
        .status()
        .unwrap();
    assert!(status.success());

    let raw = fs::read_to_string(&log).unwrap();
    // One recorded invocation after another, args NUL-separated.
    let flat: Vec<&str> = raw.trim_end_matches('\0').split('\0').collect();
    let agent_at = flat.iter().position(|a| *a == "agent").unwrap();
    let attach_at = flat.iter().position(|a| *a == "attach").unwrap();
    assert_eq!(
        &flat[agent_at..attach_at],
        [
            "agent",
            "pi",
            "--prepare",
            "--name",
            name.as_str(),
            "--workspace",
            work.to_str().unwrap(),
        ]
    );
    assert_eq!(&flat[attach_at..attach_at + 2], ["attach", name.as_str()]);
    assert!(
        flat[attach_at..].windows(2).any(|w| w == ["sh", "pi"]),
        "attach must exec the selected agent: {flat:?}"
    );

    let _ = fs::remove_dir_all(home);
}
