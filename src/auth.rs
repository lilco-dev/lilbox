//! Stored tailscale OAuth credentials, managed by `lilbox auth`.
//!
//! The credential lives in `~/.config/lilbox/auth.json` (mode 0600), kept out
//! of `config.toml` so the config file stays non-secret and shareable. The
//! join resolution order is: env (`authKeyEnv` / `oauthClientSecretEnv`) wins
//! over the stored file, which beats nothing. Storing a credential here is
//! what lets non-interactive callers — the gateway daemon above all — join
//! boxes to the tailnet without a shell environment.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::util::restrict_mode;

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct AuthFile {
    pub(crate) tailscale: Option<TailscaleAuth>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TailscaleAuth {
    #[serde(rename = "oauthClientId")]
    pub(crate) oauth_client_id: String,
    #[serde(rename = "oauthClientSecret")]
    pub(crate) oauth_client_secret: String,
}

pub(crate) fn auth_path(app: &App) -> PathBuf {
    app.config_dir.join("auth.json")
}

/// Read the stored credential, if any. A malformed file warns and reads as
/// absent — a corrupt auth store must never break box operations.
pub(crate) fn load(app: &App) -> Option<AuthFile> {
    let path = auth_path(app);
    let raw = fs::read_to_string(&path).ok()?;
    match serde_json::from_str(&raw) {
        Ok(auth) => Some(auth),
        Err(error) => {
            eprintln!("warning: ignoring unreadable {}: {error}", path.display());
            None
        }
    }
}

/// Borrowed view used by join resolution: (client id, client secret).
pub(crate) fn stored_oauth(app: &App) -> Option<(String, String)> {
    load(app)
        .and_then(|a| a.tailscale)
        .map(|t| (t.oauth_client_id, t.oauth_client_secret))
}

/// Write the credential owner-only, creating the config dir private.
pub(crate) fn store(app: &App, tailscale: TailscaleAuth) -> Result<()> {
    fs::create_dir_all(&app.config_dir)?;
    restrict_mode(&app.config_dir, 0o700);
    let path = auth_path(app);
    let file = AuthFile {
        tailscale: Some(tailscale),
    };
    let body = serde_json::to_string_pretty(&file).context("could not encode auth.json")?;
    fs::write(&path, body).with_context(|| format!("could not write {}", path.display()))?;
    restrict_mode(&path, 0o600);
    Ok(())
}

/// Remove the stored credential. Returns whether one existed.
pub(crate) fn clear(app: &App) -> Result<bool> {
    let path = auth_path(app);
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("could not remove {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_credentials() {
        let json = r#"{"tailscale":{"oauthClientId":"k123","oauthClientSecret":"shh"}}"#;
        let file: AuthFile = serde_json::from_str(json).unwrap();
        let ts = file.tailscale.unwrap();
        assert_eq!(ts.oauth_client_id, "k123");
        assert_eq!(ts.oauth_client_secret, "shh");
    }

    #[test]
    fn missing_tailscale_section_defaults_to_none() {
        let file: AuthFile = serde_json::from_str("{}").unwrap();
        assert!(file.tailscale.is_none());
    }
}
