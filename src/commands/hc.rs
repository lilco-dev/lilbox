use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::cli::HcCommand;

/// Entrypoint script the template execs; `command:` in the installed template
/// points at the installed copy's absolute path, so the hc core never depends
/// on PATH.
const AGENT_SCRIPT: &str = include_str!("../../contrib/hc/bin/lilbox-hc-agent");

/// Template body; `{command}` is replaced with the installed script path.
/// Kept in sync with contrib/hc/templates/lilbox-agent.md (the review copy).
const TEMPLATE: &str = "---
description: Coding agent inside a lilbox microVM for the current directory — one box per path, KVM-isolated, real working/blocked/done state over OSC 7501
command: {command}
---

Run a coding agent inside a lilbox microVM bound to the current directory.
Requires `lilbox` (KVM host) and `jq`. Config via `LILBOX_BIN`,
`LILBOX_AGENT_CMD` (default `claude`), and `LILBOX_IMAGE` (default `python`).
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
        HcCommand::Install => install(),
    }
}

fn install() -> Result<i32> {
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

    let template_path = template_dir.join("lilbox-agent.md");
    let body = TEMPLATE.replace("{command}", &script_path.to_string_lossy());
    fs::write(&template_path, body)
        .with_context(|| format!("could not write {}", template_path.display()))?;

    println!("installed herdr-core template 'lilbox-agent'");
    println!("  script:   {}", script_path.display());
    println!("  template: {}", template_path.display());
    println!();
    println!("requires `jq`, and `lilbox` on a KVM host; then:");
    println!("  hc run lilbox-agent \"your task\"   # from the directory to box up");
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
}
