use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Default, Deserialize)]
pub(crate) struct Config {
    pub(crate) image: Option<String>,
    pub(crate) port: Option<u16>,
    pub(crate) cpus: Option<u8>,
    pub(crate) memory: Option<String>,
    pub(crate) ttl: Option<String>,
    pub(crate) idle_timeout: Option<String>,
    pub(crate) max_cpus: Option<u8>,
    pub(crate) max_memory: Option<String>,
    #[serde(default)]
    pub(crate) tailscale: TailscaleConfig,
    #[serde(default)]
    pub(crate) gateway: GatewayConfig,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct GatewayConfig {
    /// Image booted by a gateway `new` when neither `--image` nor a template
    /// picks one. Sits between the template image and the global `image`
    /// default, and only applies on the gateway path (see `commands::gateway`).
    pub(crate) image: Option<String>,
    /// Listen address for `lilbox gateway` as a standalone SSH server (e.g.
    /// "0.0.0.0:2222"). Set by `lilbox gateway --listen`; when absent (and no
    /// flag), gateway runs as an SSH forced command.
    pub(crate) listen: Option<String>,
    /// authorized_keys-format file of client keys accepted by the gateway SSH
    /// server. Default: ~/.config/lilbox/gateway_authorized_keys.
    pub(crate) authorized_keys: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct TailscaleConfig {
    pub(crate) tag: Option<String>,
    #[serde(rename = "authKeyEnv")]
    pub(crate) auth_key_env: Option<String>,
    #[serde(rename = "oauthClientId")]
    pub(crate) oauth_client_id: Option<String>,
    #[serde(rename = "oauthClientSecretEnv")]
    pub(crate) oauth_client_secret_env: Option<String>,
    /// When `true`, `lilbox new` behaves as if `--tailnet` were always passed.
    #[serde(default)]
    pub(crate) auto: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let config: Config =
            toml::from_str("image = 'alpine'\nport = 8080\ncpus = 2\nmemory = '2G'\n").unwrap();
        assert_eq!(config.image.as_deref(), Some("alpine"));
        assert_eq!(config.port, Some(8080));
    }

    #[test]
    fn parses_tailscale_table() {
        let config: Config = toml::from_str(
            "[tailscale]\ntag = 'tag:custom'\nauthKeyEnv = 'MY_KEY'\noauthClientId = 'client-123'\noauthClientSecretEnv = 'MY_SECRET'\nauto = true\n",
        )
        .unwrap();
        assert_eq!(config.tailscale.tag.as_deref(), Some("tag:custom"));
        assert_eq!(config.tailscale.auth_key_env.as_deref(), Some("MY_KEY"));
        assert_eq!(
            config.tailscale.oauth_client_id.as_deref(),
            Some("client-123")
        );
        assert_eq!(
            config.tailscale.oauth_client_secret_env.as_deref(),
            Some("MY_SECRET")
        );
        assert_eq!(config.tailscale.auto, Some(true));
    }

    #[test]
    fn tailscale_table_is_optional() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.tailscale.tag.is_none());
        assert!(config.tailscale.auth_key_env.is_none());
        assert!(config.tailscale.oauth_client_id.is_none());
        assert!(config.tailscale.oauth_client_secret_env.is_none());
        assert!(config.tailscale.auto.is_none());
    }

    #[test]
    fn parses_gateway_table() {
        let config: Config = toml::from_str("[gateway]\nimage = 'lilbox-box'\n").unwrap();
        assert_eq!(config.gateway.image.as_deref(), Some("lilbox-box"));
    }

    #[test]
    fn gateway_table_is_optional() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.gateway.image.is_none());
    }

    #[test]
    fn gateway_table_present_but_image_absent() {
        let config: Config = toml::from_str("[gateway]\n").unwrap();
        assert!(config.gateway.image.is_none());
    }
}
