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

pub(crate) async fn url(app: &App, name: String) -> Result<()> {
    let row = app.require_row(&name)?;
    let base = row
        .display_url()
        .or_else(|| row.host_port.map(|p| format!("http://127.0.0.1:{p}/")));
    // A template can declare url_command: run it in the guest and rewrite the
    // loopback URL it prints (often token-bearing) onto the box's reachable
    // address. A stopped box or a failed command falls back to the plain URL.
    if let Some(cmd) = row
        .template
        .as_deref()
        .and_then(|t| app.template(t).ok())
        .and_then(|t| t.manifest.url_command)
        && let Ok(sandbox) = connect_box(app, &name, false).await
        && let Some(url) = guest_sign_in_url(&sandbox, &cmd, base.as_deref()).await
    {
        println!("{url}");
        return Ok(());
    }
    println!(
        "{}",
        row.display_url()
            .ok_or_else(|| anyhow!("'{name}' is not exposed (lilbox expose {name})"))?
    );
    Ok(())
}

/// Run a template's `url_command` in the guest and rewrite the URL it prints
/// onto `base` (the box's MagicDNS/expose URL, or the host port-forward).
/// Best-effort: any failure reads as `None` so callers can fall back.
pub(crate) async fn guest_sign_in_url(
    sandbox: &microsandbox::Sandbox,
    url_command: &str,
    base: Option<&str>,
) -> Option<String> {
    let output = sandbox.exec("/bin/sh", ["-c", url_command]).await.ok()?;
    if !output.status().success {
        return None;
    }
    rewrite_guest_url(&output.stdout().ok()?, base)
}

/// Extract the first http(s) URL in `raw` and, when its authority is a guest
/// loopback address and `base` is known, swap scheme+authority for `base`,
/// preserving the path and query (which carry e.g. voxl's sign-in token).
pub(crate) fn rewrite_guest_url(raw: &str, base: Option<&str>) -> Option<String> {
    let start = raw
        .find("http://")
        .map(|i| (i, "http://"))
        .into_iter()
        .chain(raw.find("https://").map(|i| (i, "https://")))
        .min_by_key(|(i, _)| *i)?;
    let (i, scheme) = start;
    let rest = &raw[i + scheme.len()..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
        .unwrap_or(rest.len());
    let authority_path = &rest[..end];
    let (authority, path_query) = match authority_path.find('/') {
        Some(s) => (&authority_path[..s], &authority_path[s..]),
        None => (authority_path, "/"),
    };
    let host = authority
        .trim_start_matches('[')
        .split([']', ':', '/'])
        .next()
        .unwrap_or(authority);
    let loopback = matches!(host, "127.0.0.1" | "localhost" | "::1");
    match (loopback, base) {
        (true, Some(base)) => Some(format!("{}{}", base.trim_end_matches('/'), path_query)),
        _ => Some(format!("{scheme}{authority_path}")),
    }
}

#[cfg(test)]
mod tests {
    use super::rewrite_guest_url;

    #[test]
    fn rewrites_loopback_onto_magicdns_keeping_token() {
        assert_eq!(
            rewrite_guest_url(
                "http://127.0.0.1:6080/?token=abc123",
                Some("https://mydesk.tail1234.ts.net/")
            ),
            Some("https://mydesk.tail1234.ts.net/?token=abc123".into())
        );
    }

    #[test]
    fn rewrites_onto_host_port_forward() {
        assert_eq!(
            rewrite_guest_url(
                "http://localhost:6080/?token=t",
                Some("http://127.0.0.1:40547/")
            ),
            Some("http://127.0.0.1:40547/?token=t".into())
        );
    }

    #[test]
    fn finds_url_embedded_in_chatter() {
        assert_eq!(
            rewrite_guest_url(
                "view http://127.0.0.1:6080/?token=x (via port publish)\n",
                Some("https://b.tail1234.ts.net/")
            ),
            Some("https://b.tail1234.ts.net/?token=x".into())
        );
    }

    #[test]
    fn leaves_non_loopback_urls_alone() {
        assert_eq!(
            rewrite_guest_url(
                "https://example.com/app?t=1",
                Some("https://b.tail1234.ts.net/")
            ),
            Some("https://example.com/app?t=1".into())
        );
    }

    #[test]
    fn keeps_loopback_when_no_base_known() {
        assert_eq!(
            rewrite_guest_url("http://127.0.0.1:6080/?token=x", None),
            Some("http://127.0.0.1:6080/?token=x".into())
        );
    }

    #[test]
    fn no_url_in_output_is_none() {
        assert_eq!(rewrite_guest_url("not a url", Some("https://b/")), None);
    }

    #[test]
    fn bare_authority_gets_root_path() {
        assert_eq!(
            rewrite_guest_url("http://127.0.0.1:6080", Some("https://b.ts.net")),
            Some("https://b.ts.net/".into())
        );
    }
}
