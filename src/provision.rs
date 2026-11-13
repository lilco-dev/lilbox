use std::{env, fs, process::Command as ProcessCommand};

use anyhow::{Result, anyhow, bail};
use microsandbox::{Image, Sandbox, sandbox::SandboxStatus};

use crate::app::App;
use crate::model::{BoxRow, Template};
use crate::sandbox::{connect_box, stop_and_remove};
use crate::tailscale::tailscale_logout_args;
use crate::util::{find_program, now, restrict_mode, run_external};

/// Run a template's setup script in a box, reconnecting to it by name. Used by
/// `lifecycle::provision_cmd` to re-run setup against an already-recorded box.
pub(crate) async fn provision(app: &App, name: &str, template: &Template) -> Result<()> {
    if template.setup.is_none() {
        return Ok(());
    }
    let sandbox = connect_box(app, name, true).await?;
    provision_sandbox(app, &sandbox, name, template).await
}

/// Run a template's setup script against an already-connected sandbox handle.
/// `cmd_new` uses this so provisioning happens before the box is recorded in
/// the DB (the row insert is the atomic commit point) — it must not call
/// `connect_box`, which would require a row that doesn't exist yet.
pub(crate) async fn provision_sandbox(
    app: &App,
    sandbox: &Sandbox,
    name: &str,
    template: &Template,
) -> Result<()> {
    let Some(setup) = &template.setup else {
        return Ok(());
    };
    fs::create_dir_all(app.logs_dir())?;
    restrict_mode(&app.logs_dir(), 0o700);
    sandbox.fs().write("/tmp/lilbox-setup.sh", setup).await?;
    println!(
        "{} provisioning {} {}",
        crate::util::cyan("◆"),
        crate::util::bold(name),
        crate::util::dim(&format!("(template {})", template.name))
    );
    let output = sandbox.exec("/bin/sh", ["/tmp/lilbox-setup.sh"]).await?;
    let mut combined = output.stdout_bytes().to_vec();
    combined.extend_from_slice(output.stderr_bytes());
    let log = app.logs_dir().join(format!("{name}-setup.log"));
    fs::write(&log, combined)?;
    // Setup output can echo secrets; keep the log owner-only.
    restrict_mode(&log, 0o600);
    if !output.status().success {
        bail!("setup failed for '{name}' (full log: {})", log.display());
    }
    println!("    {:9} {}", crate::util::dim("log"), log.display());
    Ok(())
}

pub(crate) async fn build_template_image(template: &Template, rebuild: bool) -> Result<String> {
    let tag = format!("lilbox/{}:latest", template.name);
    if !rebuild && Image::get(&tag).await.is_ok() {
        return Ok(tag);
    }
    let dir = template
        .dir
        .as_ref()
        .ok_or_else(|| anyhow!("built-in template has no Dockerfile directory"))?;
    let docker = find_program("docker")
        .ok_or_else(|| anyhow!("docker not found (needed to build template image)"))?;
    let status = ProcessCommand::new(&docker)
        .args(["build", "-t", &tag])
        .arg(dir)
        .status()?;
    if !status.success() {
        bail!("docker build failed for template '{}'", template.name);
    }
    let archive =
        env::temp_dir().join(format!("lilbox-image-{}-{}.tar", std::process::id(), now()));
    let status = ProcessCommand::new(&docker)
        .args(["save", "-o"])
        .arg(&archive)
        .arg(&tag)
        .status()?;
    if !status.success() {
        bail!("docker save failed for '{tag}'");
    }
    let loaded = Image::load(&archive, vec![tag.clone()]).await;
    let _ = fs::remove_file(&archive);
    loaded?;
    Ok(tag)
}

pub(crate) async fn teardown(app: &App, row: &BoxRow) -> Result<()> {
    if let (Some(ts), Some(port)) = (&app.tailscale, row.serve_port) {
        let verb = if row.public { "funnel" } else { "serve" };
        let _ = run_external(ts, &[verb, &format!("--https={port}"), "off"]);
    }
    if row.tailscale_node.is_some() {
        best_effort_tailnet_logout(&row.name).await;
    }
    stop_and_remove(&row.name).await?;
    app.db
        .execute("DELETE FROM boxes WHERE name=?1", [&row.name])?;
    Ok(())
}

/// Best-effort: if the box is currently running, log its tailnet node out so
/// an ephemeral node deregisters immediately instead of waiting to go
/// offline. Never resumes a stopped box just to log it out, and any failure
/// here (box not gettable, not running, exec error, non-zero exit) is only
/// warned to stderr -- it must never block `stop_and_remove`/the DB delete, nor
/// `net::leave`'s clearing of the recorded node.
pub(crate) async fn best_effort_tailnet_logout(name: &str) {
    let result: Result<()> = async {
        let handle = Sandbox::get(name).await?;
        match handle.status_snapshot() {
            SandboxStatus::Running | SandboxStatus::Draining => {}
            _ => return Ok(()),
        }
        let sandbox = handle.connect().await?;
        let output = sandbox.exec("tailscale", tailscale_logout_args()).await?;
        if !output.status().success {
            bail!("tailscale logout exited non-zero");
        }
        Ok(())
    }
    .await;
    if let Err(err) = result {
        eprintln!("warning: could not log out tailnet node for '{name}': {err:#}");
    }
}
