use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone)]
pub(crate) struct BoxRow {
    pub(crate) name: String,
    pub(crate) image: String,
    pub(crate) guest_port: Option<u16>,
    pub(crate) host_port: Option<u16>,
    pub(crate) serve_port: Option<u16>,
    pub(crate) public: bool,
    pub(crate) url: Option<String>,
    pub(crate) template: Option<String>,
    pub(crate) volume: Option<String>,
    pub(crate) expires: Option<i64>,
    pub(crate) stopped_reason: Option<String>,
    pub(crate) created: Option<String>,
    pub(crate) tailscale_node: Option<String>,
}

impl BoxRow {
    /// The box's preferred display URL: its MagicDNS URL if it joined the
    /// tailnet, else its expose/serve URL.
    pub(crate) fn display_url(&self) -> Option<String> {
        crate::tailscale::box_display_url(self.tailscale_node.as_deref(), self.url.as_deref())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct TemplateManifest {
    pub(crate) image: Option<String>,
    pub(crate) cpus: Option<u8>,
    pub(crate) memory: Option<String>,
    pub(crate) port: Option<u16>,
    /// Guest command that prints the app's sign-in URL (e.g. a token-bearing
    /// loopback URL). `lilbox url` and the tail end of `lilbox new` run it and
    /// rewrite the host onto the box's reachable address.
    pub(crate) url_command: Option<String>,
    #[serde(default)]
    pub(crate) description: String,
}

#[derive(Debug, Clone)]
pub(crate) struct Template {
    pub(crate) name: String,
    pub(crate) source: &'static str,
    pub(crate) dir: Option<PathBuf>,
    pub(crate) manifest: TemplateManifest,
    pub(crate) setup: Option<String>,
    pub(crate) dockerfile: bool,
}
