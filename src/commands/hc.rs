use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::cli::HcCommand;

/// Entrypoint script both templates exec; `command:` in the installed template
/// points at the installed copy's absolute path, so the hc core never depends
/// on PATH.
const AGENT_SCRIPT: &str = include_str!("../../contrib/hc/bin/lilbox-hc-agent");

/// Local-flavor template body; `{command}` is replaced with the installed
/// script path. Kept in sync with contrib/hc/templates/lilbox-agent.md (the
/// review copy).
const TEMPLATE: &str = "---
description: Coding agent inside a lilbox microVM for the current directory — one box per path, KVM-isolated, real working/blocked/done state over OSC 7501
command: {command}
---

Run a coding agent inside a lilbox microVM bound to the current directory.
Requires `lilbox` (KVM host) and `jq`. Config via `LILBOX_BIN`,
`LILBOX_AGENT_CMD` (default `claude`), and `LILBOX_IMAGE` (default `python`).
";

/// Remote-flavor template body: same entrypoint with LILBOX_SSH_HOST set, so
/// the box boots over the lilbox SSH gateway instead of on this host. Kept in
/// sync with contrib/hc/templates/lilbox-remote.md (the review copy).
const REMOTE_TEMPLATE: &str = "---
description: Coding agent inside a lilbox microVM on a remote gateway host — one box per directory, no local KVM needed, real working/blocked/done state over OSC 7501
command: LILBOX_SSH_HOST={host} {command}
---

Run a coding agent inside a lilbox microVM on {host} (the lilbox SSH
gateway), bound to the current directory by name. Requires `ssh` access to
the gateway and `jq`; the gateway host needs KVM and lilbox. The box's home
volume persists agent installs and logins across runs.
";

fn xdg_dir(var: &str, fallback: impl FnOnce(&std::path::Path) -> PathBuf) -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(var).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(fallback(&home))
}

pub(crate) fn cmd_hc(command: HcCommand) -> Result<i32> {
    match command {
        HcCommand::Install { remote_host } => install(remote_host.as_deref()),
    }
}

fn install(remote_host: Option<&str>) -> Result<i32> {
    let script_dir = xdg_dir("XDG_DATA_HOME", |home| home.join(".local/share"))?.join("lilbox/hc");
    let template_dir =
        xdg_dir("XDG_CONFIG_HOME", |home| home.join(".config"))?.join("herdr-core/templates");
    fs::create_dir_all(&script_dir)
        .with_context(|| format!("could not create {}", script_dir.display()))?;
    fs::create_dir_all(&template_dir)
        .with_context(|| format!("could not create {}", template_dir.display()))?;

    let script_path = script_dir.join("lilbox-hc-agent");
    fs::write(&script_path, AGENT_SCRIPT)
        .with_context(|| format!("could not write {}", script_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&script_path)?.permissions().mode() | 0o111;
        fs::set_permissions(&script_path, fs::Permissions::from_mode(mode))?;
    }

    let script = script_path.to_string_lossy();
    let template_path = template_dir.join("lilbox-agent.md");
    let body = TEMPLATE.replace("{command}", &script);
    fs::write(&template_path, body)
        .with_context(|| format!("could not write {}", template_path.display()))?;

    println!("installed herdr-core template 'lilbox-agent'");
    println!("  script:   {}", script_path.display());
    println!("  template: {}", template_path.display());

    let remote_path = template_dir.join("lilbox-remote.md");
    match remote_host {
        Some(host) => {
            let body = REMOTE_TEMPLATE
                .replace("{host}", host)
                .replace("{command}", &script);
            fs::write(&remote_path, body)
                .with_context(|| format!("could not write {}", remote_path.display()))?;
            println!("installed herdr-core template 'lilbox-remote' (gateway {host})");
            println!("  template: {}", remote_path.display());
        }
        None => {
            // Drop a stale remote template rather than leave one pointing at a
            // host the user didn't ask for this time.
            if remote_path.exists() {
                fs::remove_file(&remote_path)?;
                println!("removed template 'lilbox-remote' (no --remote-host given)");
            }
        }
    }

    println!();
    println!("requires `jq`; then:");
    println!("  hc run lilbox-agent \"your task\"    # box on this host (needs KVM)");
    if remote_host.is_some() {
        println!("  hc run lilbox-remote \"your task\"   # box on the gateway host");
    } else {
        println!("  lilbox hc install --remote-host <user@host>   # add a gateway flavor");
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_references_script_only_via_command_line() {
        // The hc core execs `command:` verbatim; any other path reference in
        // the body would be documentation drift.
        let body = TEMPLATE.replace("{command}", "/x/lilbox-hc-agent");
        assert!(body.contains("command: /x/lilbox-hc-agent"));
        assert!(
            !body
                .replace("command: /x/lilbox-hc-agent", "")
                .contains("/x/")
        );
    }

    #[test]
    fn remote_template_carries_host_env_prefix() {
        let body = REMOTE_TEMPLATE
            .replace("{host}", "lilbox@example.com")
            .replace("{command}", "/x/lilbox-hc-agent");
        assert!(body.contains("command: LILBOX_SSH_HOST=lilbox@example.com /x/lilbox-hc-agent"));
        assert!(!body.contains("{host}"));
        assert!(!body.contains("{command}"));
    }
}
