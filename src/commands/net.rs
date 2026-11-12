use anyhow::{Result, anyhow};
use rusqlite::params;

use crate::app::App;
use crate::provision::best_effort_tailnet_logout;
use crate::sandbox::connect_box;
use crate::tailscale::{allocate_serve_port, join_box, resolve_join_credential, tailnet_host};
use crate::util::{DEFAULT_GUEST_PORT, run_external, successful_output};

/// Join a box's guest to the tailnet as its own node, recording the MagicDNS
/// name it comes back with.
///
/// Idempotent: a box already recorded as joined is a no-op success. Unlike the
/// join folded into `lilbox new`, every failure here is fatal — the user asked
/// for this join specifically, so silently booting on without it would be a lie.
pub(crate) async fn join(app: &App, name: String, tailnet_tag: Option<String>) -> Result<()> {
    let row = app.require_row(&name)?;
    if let Some(node) = &row.tailscale_node {
        println!("'{name}' is already on the tailnet at {node}");
        return Ok(());
    }
    let config = app.config()?;
    let stored = crate::auth::stored_oauth(app);
    let credential = resolve_join_credential(
        &config.tailscale,
        tailnet_tag.as_deref(),
        &name,
        stored.as_ref().map(|(id, secret)| (id.as_str(), secret.as_str())),
    )
        .await?
        .ok_or_else(|| {
            anyhow!(
                "no tailnet auth key available — set TS_AUTHKEY, or configure [tailscale] oauthClientId + {}",
                crate::tailscale::DEFAULT_OAUTH_SECRET_ENV
            )
        })?;
    let sandbox = connect_box(app, &name, true).await?;
    let guest_port = row.guest_port.unwrap_or(DEFAULT_GUEST_PORT);
    let node = join_box(
        &sandbox,
        &credential.tag,
        &credential.key_env,
        credential.key,
        &name,
        guest_port,
        &row.image,
    )
    .await?;
    app.db.execute(
        "UPDATE boxes SET tailscale_node=?1 WHERE name=?2",
        params![node, name],
    )?;
    println!("{name} joined the tailnet:\n    https://{node}/");
    Ok(())
}

/// Log a box's guest out of the tailnet and forget its node.
///
/// Idempotent: a box with no recorded node is a no-op success. The logout
/// itself is best-effort (a stopped box can't be logged out, and an ephemeral
/// node deregisters on its own), but the DB always ends up clean so `fork` and
/// `join` see accurate state.
pub(crate) async fn leave(app: &App, name: String) -> Result<()> {
    let row = app.require_row(&name)?;
    if row.tailscale_node.is_none() {
        println!("'{name}' is not on the tailnet");
        return Ok(());
    }
    best_effort_tailnet_logout(&name).await;
    app.db.execute(
        "UPDATE boxes SET tailscale_node=NULL WHERE name=?1",
        [&name],
    )?;
    println!("{name} left the tailnet");
    Ok(())
}

pub(crate) fn expose(app: &App, name: String, public: bool) -> Result<()> {
    let ts = app
        .tailscale
        .as_deref()
        .ok_or_else(|| anyhow!("tailscale not found - cannot publish"))?;
    let row = app.require_row(&name)?;
    if let Some(url) = row.url {
        println!("already exposed at {url}");
        return Ok(());
    }
    let host_port = row
        .host_port
        .ok_or_else(|| anyhow!("box '{name}' has no published port"))?;
    let port = allocate_serve_port(app, public)?;
    let verb = if public { "funnel" } else { "serve" };
    successful_output(
        ts,
        &[
            verb,
            "--bg",
            &format!("--https={port}"),
            &format!("http://127.0.0.1:{host_port}"),
        ],
    )?;
    let host = tailnet_host(ts).unwrap_or_else(|| "this-host".into());
    let suffix = if public && port == 443 {
        String::new()
    } else {
        format!(":{port}")
    };
    let url = format!("https://{host}{suffix}/");
    app.db.execute(
        "UPDATE boxes SET serve_port=?1,public=?2,url=?3 WHERE name=?4",
        params![port, public as i32, url, name],
    )?;
    println!(
        "{name} is live {}:\n    {url}",
        if public {
            "publicly"
        } else {
            "on your tailnet"
        }
    );
    Ok(())
}

pub(crate) fn unexpose(app: &App, name: String) -> Result<()> {
    let row = app.require_row(&name)?;
    let Some(port) = row.serve_port else {
        println!("'{name}' is not exposed");
        return Ok(());
    };
    if let Some(ts) = &app.tailscale {
        let verb = if row.public { "funnel" } else { "serve" };
        let _ = run_external(ts, &[verb, &format!("--https={port}"), "off"]);
    }
    app.db.execute(
        "UPDATE boxes SET serve_port=NULL,public=0,url=NULL WHERE name=?1",
        [&name],
    )?;
    println!("unpublished {name}");
    Ok(())
}

pub(crate) fn url(app: &App, name: String) -> Result<()> {
    let row = app.require_row(&name)?;
    println!(
        "{}",
        row.display_url()
            .ok_or_else(|| anyhow!("'{name}' is not exposed (lilbox expose {name})"))?
    );
    Ok(())
}
