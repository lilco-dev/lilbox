//! `lilbox auth` — store, inspect, and remove the tailnet OAuth credential.

use std::io::{Read, Write, stdin, stdout};

use anyhow::{Context, Result, bail};

use crate::app::App;
use crate::auth::{self, TailscaleAuth};
use crate::cli::AuthCommand;
use crate::tailscale;

pub(crate) async fn cmd_auth(app: &App, command: AuthCommand) -> Result<()> {
    match command {
        AuthCommand::Login {
            client_id,
            client_secret_stdin,
            no_verify,
        } => login(app, client_id, client_secret_stdin, no_verify).await,
        AuthCommand::Logout => logout(app),
        AuthCommand::Status => status(app),
    }
}

async fn login(
    app: &App,
    client_id: Option<String>,
    secret_stdin: bool,
    no_verify: bool,
) -> Result<()> {
    let client_id = match client_id {
        Some(id) => id,
        None => prompt("Tailscale OAuth client ID: ")?,
    };
    if client_id.trim().is_empty() {
        bail!("client ID cannot be empty");
    }
    let secret = if secret_stdin {
        let mut buf = String::new();
        stdin()
            .read_to_string(&mut buf)
            .context("could not read client secret from stdin")?;
        buf.trim().to_string()
    } else {
        rpassword::prompt_password("Tailscale OAuth client secret: ")
            .context("could not read client secret")?
    };
    if secret.is_empty() {
        bail!("client secret cannot be empty");
    }
    if !no_verify {
        tailscale::check_oauth_client(&client_id, &secret).await?;
    }
    auth::store(
        app,
        TailscaleAuth {
            oauth_client_id: client_id.trim().to_string(),
            oauth_client_secret: secret,
        },
    )?;
    println!(
        "stored tailnet credential in {} (mode 0600)",
        auth::auth_path(app).display()
    );
    println!(
        "boxes created with --tailnet will use it; set [tailscale] auto = true to join every box"
    );
    Ok(())
}

fn logout(app: &App) -> Result<()> {
    if auth::clear(app)? {
        println!("removed the stored tailnet credential");
    } else {
        println!("no stored credential (env-based credentials are unaffected)");
    }
    Ok(())
}

fn status(app: &App) -> Result<()> {
    let config = app.config()?;
    let mut any = false;
    if let Some(ts) = auth::load(app).and_then(|a| a.tailscale) {
        any = true;
        println!(
            "stored:      oauth client {} ({})",
            ts.oauth_client_id,
            auth::auth_path(app).display()
        );
    }
    if let Some(id) = &config.tailscale.oauth_client_id {
        any = true;
        let env_name = config
            .tailscale
            .oauth_client_secret_env
            .as_deref()
            .unwrap_or(tailscale::DEFAULT_OAUTH_SECRET_ENV);
        let state = if std::env::var(env_name).is_ok_and(|v| !v.is_empty()) {
            "secret set"
        } else {
            "secret NOT set"
        };
        println!("config env:  oauth client {id} (secret from ${env_name}, {state})");
    }
    let key_env = config
        .tailscale
        .auth_key_env
        .as_deref()
        .unwrap_or(tailscale::DEFAULT_AUTH_KEY_ENV);
    if std::env::var(key_env).is_ok_and(|v| !v.is_empty()) {
        any = true;
        println!("static key:  ${key_env} is set");
    }
    if !any {
        println!("no tailnet credential configured — boxes cannot join the tailnet");
        println!("fix: lilbox auth login, or set ${key_env}");
    }
    println!("resolution:  config env > stored > static key env");
    Ok(())
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    stdout().flush()?;
    let mut line = String::new();
    stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}
