use std::{collections::HashSet, env, path::Path};

use anyhow::{Context, Result, anyhow, bail};
use microsandbox::Sandbox;

use crate::app::App;
use crate::config::TailscaleConfig;
use crate::util::successful_output;

const SERVE_PORT_BASE: u16 = 8443;
const SERVE_PORT_MAX: u16 = 8480;

const DEFAULT_TAG: &str = "tag:lilbox-vm";
pub(crate) const DEFAULT_AUTH_KEY_ENV: &str = "TS_AUTHKEY";

pub(crate) const DEFAULT_OAUTH_SECRET_ENV: &str = "TS_OAUTH_CLIENT_SECRET";
const TAILSCALE_API_BASE: &str = "https://api.tailscale.com/api/v2";
const MINTED_KEY_EXPIRY_SECS: u64 = 300;

pub(crate) fn tailnet_host(ts: &Path) -> Option<String> {
    let out = successful_output(ts, &["status", "--json"]).ok()?;
    node_hostname(&out)
}

/// Parse `Self.DNSName` out of `tailscale status --json`, trailing dot trimmed.
/// Tolerates leading noise (e.g. a stray line printed before the JSON object)
/// by scanning for the first `{` rather than assuming the whole string is JSON.
pub(crate) fn node_hostname(json: &str) -> Option<String> {
    let start = json.find('{')?;
    serde_json::from_str::<serde_json::Value>(&json[start..]).ok()?["Self"]["DNSName"]
        .as_str()
        .map(|s| s.trim_end_matches('.').into())
}

/// flag > config > default.
pub(crate) fn resolve_tag(flag: Option<&str>, config_tag: Option<&str>) -> String {
    flag.or(config_tag).unwrap_or(DEFAULT_TAG).to_string()
}

pub(crate) fn validate_tag(tag: &str) -> Result<()> {
    let Some(name) = tag.strip_prefix("tag:") else {
        bail!("invalid tailnet tag '{tag}' (must start with 'tag:')");
    };
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        bail!("invalid tailnet tag '{tag}' (name must be non-empty and use [A-Za-z0-9-])");
    }
    Ok(())
}

pub(crate) fn require_auth_key(value: Option<String>, key_env: &str) -> Result<String> {
    match value {
        Some(value) if !value.is_empty() => Ok(value),
        _ => bail!("{key_env} is not set"),
    }
}

/// `key_env` is interpolated (not passed positionally) into the guest script,
/// so it must look like a POSIX environment variable name before use.
pub(crate) fn is_valid_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Build the guest exec argv for joining the tailnet. References the key only
/// via the env var name -- the literal key value never appears here.
///
/// After a successful `tailscale up`, best-effort `tailscale serve`s
/// `guest_port` on 443 so the box's MagicDNS URL proxies to the app. Serve
/// failures only warn to stderr -- they must not fail the join.
pub(crate) fn tailscale_up_command(
    tag: &str,
    key_env: &str,
    hostname: &str,
    guest_port: u16,
) -> Vec<String> {
    let script = format!(
        "command -v tailscaled >/dev/null 2>&1 || exit 3; /usr/local/bin/lilbox-boot || exit 4; \
tailscale up --auth-key=\"${key_env}\" --advertise-tags=\"$1\" --ssh --hostname=\"$2\" || exit 5; \
tailscale serve --bg \"$3\" || echo \"warning: tailscale serve failed\" >&2; \
tailscale status --json"
    );
    vec![
        "/bin/sh".into(),
        "-c".into(),
        script,
        "sh".into(),
        tag.into(),
        hostname.into(),
        guest_port.to_string(),
    ]
}

/// Explain a failed tailnet join from the guest exit code + stderr.
///
/// `stderr` wins when non-empty (it's the real `tailscale up` error for exit
/// 5). Otherwise the message is keyed on the exit code the join script in
/// [`tailscale_up_command`] uses: `3` = no `tailscaled` in the image, `4` =
/// `lilbox-boot` (tailscaled start) failed.
pub(crate) fn join_failure_detail(code: i32, stderr: &str, image: &str) -> String {
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        return stderr.to_string();
    }
    match code {
        3 => format!(
            "image '{image}' has no tailscaled — use --image lilbox-box (or an image with Tailscale baked in)"
        ),
        4 => "tailscaled failed to start in the guest".to_string(),
        _ => format!("tailscale up exited with status {code}"),
    }
}

/// Join a box's guest to the tailnet, returning its MagicDNS node name.
///
/// Both `lilbox new` and `lilbox join` route through here, but they treat
/// failure differently and deliberately so: `new` warns and keeps the box (a
/// tailnet hiccup must never abort a boot), while `join` propagates the error,
/// because there the join *is* the requested operation. That asymmetry lives in
/// the callers -- this function always reports failure.
///
/// Tailscale requires the guest to present its own auth key to `tailscale up`;
/// there is no host-side "join for me" call, so the box necessarily sees the
/// real key. It is delivered as a transient env var on this one exec (never
/// through the builder, so it is never written into the sandbox's persisted
/// config/state). Prefer ephemeral, single-use keys -- the OAuth mint path
/// already does -- so the guest's momentary visibility of its own key is moot.
pub(crate) async fn join_box(
    sandbox: &Sandbox,
    tag: &str,
    key_env: &str,
    auth_key: String,
    name: &str,
    guest_port: u16,
    image: &str,
) -> Result<String> {
    let argv = tailscale_up_command(tag, key_env, name, guest_port);
    let result = sandbox
        .exec_with(argv[0].clone(), |e| {
            e.args(argv[1..].to_vec()).env(key_env, auth_key.as_str())
        })
        .await;
    drop(auth_key);
    let output = result.with_context(|| format!("could not join tailnet for '{name}'"))?;
    if !output.status().success {
        bail!(
            "could not join tailnet for '{name}': {}",
            join_failure_detail(
                output.status().code,
                &output.stderr().unwrap_or_default(),
                image,
            )
        );
    }
    output
        .stdout()
        .ok()
        .as_deref()
        .and_then(node_hostname)
        .ok_or_else(|| {
            anyhow!("'{name}' joined the tailnet but its node name could not be read back")
        })
}

/// A resolved credential for one tailnet join: the tag to advertise, the env
/// var name that carries the key into the guest, and the key itself.
pub(crate) struct JoinCredential {
    pub(crate) tag: String,
    pub(crate) key_env: String,
    pub(crate) key: String,
}

/// Resolve a tag and auth key for a single join, minting an ephemeral key when
/// an OAuth client is configured. `Ok(None)` means no credential is available
/// at all (`JoinMode::Skip`) -- the caller decides whether that is fatal.
///
/// `cmd_new` deliberately does *not* use this: it walks the same
/// [`resolve_join_mode`] decision but warns and continues at every step, where
/// this bails. Sharing the policy while diverging on error handling is the
/// intended seam; collapsing the two would drag `new`'s never-fail rule into
/// `join`, where it would be wrong.
pub(crate) async fn resolve_join_credential(
    cfg: &TailscaleConfig,
    flag_tag: Option<&str>,
    box_name: &str,
    stored: Option<(&str, &str)>,
) -> Result<Option<JoinCredential>> {
    match resolve_join_mode(cfg, flag_tag, |name| env::var(name).ok(), stored) {
        JoinMode::Mint {
            tag,
            client_id,
            client_secret,
        } => {
            validate_tag(&tag)?;
            let key = mint_ephemeral_key(&client_id, &client_secret, &tag, box_name).await?;
            Ok(Some(JoinCredential {
                tag,
                key_env: DEFAULT_AUTH_KEY_ENV.to_string(),
                key,
            }))
        }
        JoinMode::StaticEnv { key_env } => {
            if !is_valid_env_var_name(&key_env) {
                bail!("tailscale authKeyEnv '{key_env}' is not a valid environment variable name");
            }
            let tag = resolve_tag(flag_tag, cfg.tag.as_deref());
            validate_tag(&tag)?;
            let key = require_auth_key(env::var(&key_env).ok(), &key_env)?;
            Ok(Some(JoinCredential { tag, key_env, key }))
        }
        JoinMode::Skip => Ok(None),
    }
}

/// Guest script that strips any tailnet identity a snapshot carried into a
/// clone: stop the daemon and delete its state file.
///
/// Deliberately does **not** run `tailscale logout`. A clone that inherited
/// state is presenting the *parent's* node identity, so logging out here would
/// deregister the parent's node. Every step tolerates failure and an image with
/// no `tailscaled` exits 0 -- this runs on clones of boxes that were never on
/// the tailnet too.
pub(crate) const TAILNET_SCRUB_SCRIPT: &str = "command -v tailscaled >/dev/null 2>&1 || exit 0; \
pkill -x tailscaled >/dev/null 2>&1 || true; \
rm -f /var/lib/tailscale/tailscaled.state; \
exit 0";

/// What `lilbox fork` should do about the parent's tailnet membership.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ForkPlan {
    /// Parent holds no tailnet identity: snapshot straight away.
    Snapshot,
    /// Parent is joined and `--force` was passed: leave, then snapshot.
    LeaveThenSnapshot,
    /// Parent is joined and `--force` was not passed: refuse.
    Refuse,
}

/// Decide whether a fork may snapshot, given the parent's recorded tailnet node
/// and `--force`.
///
/// A snapshot captures the guest's `/var/lib/tailscale`, so forking a joined box
/// boots a clone presenting the parent's node key -- two VMs, one identity, both
/// reachable over Tailscale SSH. Pure so the guard is testable without a KVM
/// host; the caller must run it before any snapshot work.
pub(crate) fn plan_fork(tailscale_node: Option<&str>, force: bool) -> ForkPlan {
    match (tailscale_node, force) {
        (None, _) => ForkPlan::Snapshot,
        (Some(_), true) => ForkPlan::LeaveThenSnapshot,
        (Some(_), false) => ForkPlan::Refuse,
    }
}

pub(crate) fn serve_ports(ts: &Path) -> HashSet<u16> {
    let mut ports = HashSet::new();
    let Ok(out) = successful_output(ts, &["serve", "status", "--json"]) else {
        return ports;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&out) else {
        return ports;
    };
    for key in ["TCP", "Web"] {
        if let Some(map) = value[key].as_object() {
            for name in map.keys() {
                if let Ok(port) = name.rsplit(':').next().unwrap_or(name).parse() {
                    ports.insert(port);
                }
            }
        }
    }
    ports
}

/// The box's own MagicDNS URL, e.g. `https://web.tail1234.ts.net/`.
fn magicdns_url(node: &str) -> String {
    format!("https://{node}/")
}

/// Preferred display URL for a box: MagicDNS if it's on the tailnet, else the
/// expose/serve URL.
pub(crate) fn box_display_url(
    tailscale_node: Option<&str>,
    expose_url: Option<&str>,
) -> Option<String> {
    tailscale_node
        .map(magicdns_url)
        .or_else(|| expose_url.map(String::from))
}

/// Args for `tailscale ssh` to a box node: ["ssh", "root@<node>", <cmd...>]
pub(crate) fn tailscale_ssh_args(node: &str, cmd: &[String]) -> Vec<String> {
    let mut args = vec!["ssh".to_string(), format!("root@{node}")];
    args.extend(cmd.iter().cloned());
    args
}

/// Guest exec argv to log a box's tailnet node out, so an ephemeral node
/// deregisters immediately instead of waiting for it to go offline.
pub(crate) fn tailscale_logout_args() -> [&'static str; 1] {
    ["logout"]
}

pub(crate) fn allocate_serve_port(app: &App, public: bool) -> Result<u16> {
    let ts = app
        .tailscale
        .as_deref()
        .ok_or_else(|| anyhow!("tailscale not found - cannot publish"))?;
    let mut used = serve_ports(ts);
    for row in app.rows()? {
        if let Some(port) = row.serve_port {
            used.insert(port);
        }
    }
    let choices: Vec<u16> = if public {
        vec![8443, 10000, 443]
    } else {
        (SERVE_PORT_BASE..=SERVE_PORT_MAX).collect()
    };
    choices
        .into_iter()
        .find(|p| !used.contains(p))
        .ok_or_else(|| anyhow!("no free Tailscale HTTPS port"))
}

/// How `lilbox new` should join (or not join) the tailnet, decided purely
/// from config + resolved env so it's testable without touching real env.
pub(crate) enum JoinMode {
    Mint {
        tag: String,
        client_id: String,
        client_secret: String,
    },
    StaticEnv {
        key_env: String,
    },
    Skip,
}

/// OAuth client (mint an ephemeral key) takes precedence over the static
/// `authKeyEnv` path. Among OAuth sources the config + env-secret pair wins
/// over the credential stored by `lilbox auth login`; the static key is the
/// last resort, and when nothing resolves the join is skipped.
pub(crate) fn resolve_join_mode(
    cfg: &TailscaleConfig,
    flag_tag: Option<&str>,
    env: impl Fn(&str) -> Option<String>,
    stored: Option<(&str, &str)>,
) -> JoinMode {
    let tag = resolve_tag(flag_tag, cfg.tag.as_deref());
    if let Some(client_id) = cfg.oauth_client_id.as_deref() {
        let secret_env = cfg
            .oauth_client_secret_env
            .as_deref()
            .unwrap_or(DEFAULT_OAUTH_SECRET_ENV);
        if let Some(client_secret) = env(secret_env).filter(|v| !v.is_empty()) {
            return JoinMode::Mint {
                tag,
                client_id: client_id.to_string(),
                client_secret,
            };
        }
    }
    if let Some((client_id, client_secret)) = stored
        && !client_secret.is_empty()
    {
        return JoinMode::Mint {
            tag,
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
        };
    }
    let key_env = cfg
        .auth_key_env
        .clone()
        .unwrap_or_else(|| DEFAULT_AUTH_KEY_ENV.into());
    if env(&key_env).is_some_and(|v| !v.is_empty()) {
        return JoinMode::StaticEnv { key_env };
    }
    JoinMode::Skip
}

/// Form-encoded body for `POST /oauth/token`.
pub(crate) fn oauth_token_form(id: &str, secret: &str) -> [(&'static str, String); 3] {
    [
        ("client_id", id.to_string()),
        ("client_secret", secret.to_string()),
        ("grant_type", "client_credentials".to_string()),
    ]
}

/// Verify an OAuth client credential by requesting (and discarding) an access
/// token. Used by `lilbox auth login`; unlike [`mint_ephemeral_key`] this
/// creates no auth key.
pub(crate) async fn check_oauth_client(client_id: &str, client_secret: &str) -> Result<()> {
    let response = reqwest::Client::new()
        .post(format!("{TAILSCALE_API_BASE}/oauth/token"))
        .form(&oauth_token_form(client_id, client_secret))
        .send()
        .await
        .context("could not reach Tailscale OAuth endpoint")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("tailscale rejected the credential ({status}): {body}");
    }
    Ok(())
}

/// JSON body for `POST /tailnet/-/keys`: a reusable=false, ephemeral,
/// preauthorized key tagged and scoped to a short expiry.
pub(crate) fn create_key_body(tag: &str, box_name: &str, expiry_secs: u64) -> serde_json::Value {
    serde_json::json!({
        "capabilities": {
            "devices": {
                "create": {
                    "reusable": false,
                    "ephemeral": true,
                    "preauthorized": true,
                    "tags": [tag],
                }
            }
        },
        "expirySeconds": expiry_secs,
        "description": format!("lilbox {box_name}"),
    })
}

pub(crate) fn parse_access_token(json: &str) -> Result<String> {
    serde_json::from_str::<serde_json::Value>(json)?["access_token"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| anyhow!("oauth token response missing 'access_token'"))
}

pub(crate) fn parse_minted_key(json: &str) -> Result<String> {
    serde_json::from_str::<serde_json::Value>(json)?["key"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| anyhow!("tailnet keys response missing 'key'"))
}

/// Mints a single ephemeral, tagged, short-lived auth key via the Tailscale
/// OAuth client-credentials flow. No caching -- called once per `new`.
pub(crate) async fn mint_ephemeral_key(
    client_id: &str,
    client_secret: &str,
    tag: &str,
    box_name: &str,
) -> Result<String> {
    let client = reqwest::Client::new();
    let token_response = client
        .post(format!("{TAILSCALE_API_BASE}/oauth/token"))
        .form(&oauth_token_form(client_id, client_secret))
        .send()
        .await
        .context("could not reach Tailscale OAuth endpoint")?;
    let status = token_response.status();
    let body = token_response
        .text()
        .await
        .context("could not read oauth token response")?;
    if !status.is_success() {
        bail!("oauth token request failed ({status}): {body}");
    }
    let access_token = parse_access_token(&body)?;

    let key_response = client
        .post(format!("{TAILSCALE_API_BASE}/tailnet/-/keys"))
        .bearer_auth(access_token)
        .json(&create_key_body(tag, box_name, MINTED_KEY_EXPIRY_SECS))
        .send()
        .await
        .context("could not reach Tailscale keys endpoint")?;
    let status = key_response.status();
    let body = key_response
        .text()
        .await
        .context("could not read tailnet keys response")?;
    if !status.is_success() {
        bail!("tailnet key creation failed ({status}): {body}");
    }
    parse_minted_key(&body)
}

/// Whether `lilbox new` should put the box on the tailnet. Opt-in: a key merely
/// present in the environment is NOT enough — the user must signal intent via
/// `--tailnet`, `--tailnet-tag`, or `[tailscale] auto = true`.
pub(crate) fn wants_tailnet(tailnet: bool, tailnet_tag: Option<&str>, auto: Option<bool>) -> bool {
    tailnet || tailnet_tag.is_some() || auto.unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wants_tailnet_needs_explicit_intent() {
        // No intent -> false. (A key present in the env is not consulted here;
        // that's the whole point of the opt-in model.)
        assert!(!wants_tailnet(false, None, None));
        assert!(!wants_tailnet(false, None, Some(false)));
        // Each intent source independently opts in.
        assert!(wants_tailnet(true, None, None));
        assert!(wants_tailnet(false, Some("tag:x"), None));
        assert!(wants_tailnet(false, None, Some(true)));
    }

    #[test]
    fn flag_overrides_config_and_default() {
        assert_eq!(
            resolve_tag(Some("tag:from-flag"), Some("tag:from-config")),
            "tag:from-flag"
        );
    }

    #[test]
    fn config_overrides_default() {
        assert_eq!(
            resolve_tag(None, Some("tag:from-config")),
            "tag:from-config"
        );
    }

    #[test]
    fn falls_back_to_default_tag() {
        assert_eq!(resolve_tag(None, None), DEFAULT_TAG);
    }

    #[test]
    fn validate_tag_accepts_prefixed_tags() {
        assert!(validate_tag("tag:lilbox-vm").is_ok());
        assert!(validate_tag("tag:my-box-1").is_ok());
    }

    #[test]
    fn validate_tag_rejects_missing_prefix() {
        assert!(validate_tag("lilbox-vm").is_err());
    }

    #[test]
    fn validate_tag_rejects_empty() {
        assert!(validate_tag("").is_err());
    }

    #[test]
    fn validate_tag_rejects_bare_prefix() {
        assert!(validate_tag("tag:").is_err());
    }

    const A_NODE: &str = "web.tail1234.ts.net";

    #[test]
    fn plan_fork_snapshots_a_box_with_no_tailnet_identity() {
        assert_eq!(plan_fork(None, false), ForkPlan::Snapshot);
        assert_eq!(plan_fork(None, true), ForkPlan::Snapshot);
    }

    #[test]
    fn plan_fork_refuses_a_joined_box() {
        // The defect this guards: a snapshot carries /var/lib/tailscale, so the
        // clone would boot presenting the parent's node key.
        assert_eq!(plan_fork(Some(A_NODE), false), ForkPlan::Refuse);
    }

    #[test]
    fn plan_fork_leaves_first_when_forced() {
        assert_eq!(plan_fork(Some(A_NODE), true), ForkPlan::LeaveThenSnapshot);
    }

    #[test]
    fn scrub_script_never_logs_out() {
        // A clone that inherited state presents the *parent's* identity, so a
        // logout here would deregister the parent's node, not the clone's.
        assert!(
            !TAILNET_SCRUB_SCRIPT.contains("logout"),
            "scrub must not log out: {TAILNET_SCRUB_SCRIPT}"
        );
    }

    #[test]
    fn scrub_script_removes_state_and_tolerates_a_plain_image() {
        assert!(TAILNET_SCRUB_SCRIPT.contains("rm -f /var/lib/tailscale/tailscaled.state"));
        // Clones of never-joined boxes run this too, so an image with no
        // tailscaled must exit 0 rather than fail the fork.
        assert!(
            TAILNET_SCRUB_SCRIPT.starts_with("command -v tailscaled >/dev/null 2>&1 || exit 0;"),
            "scrub must short-circuit on images without tailscaled"
        );
    }

    #[test]
    fn require_auth_key_errs_when_unset() {
        let err = require_auth_key(None, "TS_AUTHKEY").unwrap_err();
        assert!(err.to_string().contains("TS_AUTHKEY"));
    }

    #[test]
    fn require_auth_key_errs_when_empty() {
        assert!(require_auth_key(Some(String::new()), "TS_AUTHKEY").is_err());
    }

    #[test]
    fn require_auth_key_ok_when_set() {
        assert_eq!(
            require_auth_key(Some("tskey-auth-test".into()), "TS_AUTHKEY").unwrap(),
            "tskey-auth-test"
        );
    }

    #[test]
    fn builds_tailscale_up_argv() {
        let argv = tailscale_up_command("tag:lilbox-vm", "TS_AUTHKEY", "mybox", 8080);
        let joined = argv.join(" ");
        assert!(joined.contains("--advertise-tags="));
        assert!(joined.contains("tag:lilbox-vm"));
        assert!(joined.contains("--ssh"));
        assert!(joined.contains("$TS_AUTHKEY"));
        assert!(!joined.contains("tskey-auth"));
        assert!(joined.contains("tailscale serve --bg"));
        assert!(joined.contains("\"$3\""));

        let script = &argv[2];
        let up_pos = script.find("tailscale up").expect("tailscale up present");
        let up_fail_pos = script[up_pos..].find("|| exit 5").expect("up hard-fails");
        let serve_pos = script
            .find("tailscale serve")
            .expect("tailscale serve present");
        assert!(
            up_pos + up_fail_pos < serve_pos,
            "up must hard-fail before serve runs"
        );
        let serve_tail = &script[serve_pos..];
        let serve_fallback_end = serve_tail.find(">&2;").expect("serve warns and continues");
        assert!(
            !serve_tail[..serve_fallback_end].contains("exit"),
            "serve step must be best-effort (no exit)"
        );

        assert_eq!(
            &argv[argv.len() - 3..],
            &[
                "tag:lilbox-vm".to_string(),
                "mybox".to_string(),
                "8080".to_string()
            ]
        );
    }

    #[test]
    fn node_hostname_parses_and_trims_trailing_dot() {
        let json = r#"{"Self":{"DNSName":"web.tail1234.ts.net."}}"#;
        assert_eq!(node_hostname(json).as_deref(), Some("web.tail1234.ts.net"));
    }

    #[test]
    fn is_valid_env_var_name_accepts_typical_names() {
        assert!(is_valid_env_var_name("TS_AUTHKEY"));
        assert!(is_valid_env_var_name("_private"));
        assert!(is_valid_env_var_name("a1"));
    }

    #[test]
    fn is_valid_env_var_name_rejects_bad_names() {
        assert!(!is_valid_env_var_name(""));
        assert!(!is_valid_env_var_name("1BAD"));
        assert!(!is_valid_env_var_name("TS AUTHKEY"));
        assert!(!is_valid_env_var_name("TS-AUTHKEY"));
        assert!(!is_valid_env_var_name("TS_AUTHKEY\"; rm -rf /"));
    }

    #[test]
    fn magicdns_url_formats_https() {
        assert_eq!(
            magicdns_url("web.tail1.ts.net"),
            "https://web.tail1.ts.net/"
        );
    }

    #[test]
    fn box_display_url_prefers_magicdns() {
        assert_eq!(
            box_display_url(Some("web.tail1.ts.net"), Some("https://exposed/")),
            Some("https://web.tail1.ts.net/".into())
        );
    }

    #[test]
    fn box_display_url_falls_back_to_expose() {
        assert_eq!(
            box_display_url(None, Some("https://exposed/")),
            Some("https://exposed/".into())
        );
    }

    #[test]
    fn box_display_url_none_when_neither() {
        assert_eq!(box_display_url(None, None), None);
    }

    #[test]
    fn tailscale_ssh_args_interactive() {
        assert_eq!(
            tailscale_ssh_args("web.tail1.ts.net", &[]),
            vec!["ssh".to_string(), "root@web.tail1.ts.net".to_string()]
        );
    }

    #[test]
    fn tailscale_ssh_args_with_command() {
        let cmd = vec!["ls".to_string(), "-la".to_string()];
        assert_eq!(
            tailscale_ssh_args("web.tail1.ts.net", &cmd),
            vec![
                "ssh".to_string(),
                "root@web.tail1.ts.net".to_string(),
                "ls".to_string(),
                "-la".to_string()
            ]
        );
    }

    #[test]
    fn tailscale_logout_args_is_just_logout() {
        assert_eq!(tailscale_logout_args(), ["logout"]);
    }

    #[test]
    fn node_hostname_ignores_leading_noise_before_json_object() {
        let output =
            "/usr/local/bin/tailscaled\n{\"Self\":{\"DNSName\":\"web.tail1234.ts.net.\"}}\n";
        assert_eq!(
            node_hostname(output).as_deref(),
            Some("web.tail1234.ts.net")
        );
    }

    fn cfg(
        tag: Option<&str>,
        auth_key_env: Option<&str>,
        oauth_client_id: Option<&str>,
        oauth_client_secret_env: Option<&str>,
    ) -> TailscaleConfig {
        TailscaleConfig {
            tag: tag.map(String::from),
            auth_key_env: auth_key_env.map(String::from),
            oauth_client_id: oauth_client_id.map(String::from),
            oauth_client_secret_env: oauth_client_secret_env.map(String::from),
            auto: None,
        }
    }

    #[test]
    fn resolve_join_mode_mints_when_oauth_id_and_secret_present() {
        let config = cfg(None, None, Some("client-123"), None);
        let mode = resolve_join_mode(
            &config,
            None,
            |name| (name == DEFAULT_OAUTH_SECRET_ENV).then(|| "shh".to_string()),
            None,
        );
        match mode {
            JoinMode::Mint {
                client_id,
                client_secret,
                ..
            } => {
                assert_eq!(client_id, "client-123");
                assert_eq!(client_secret, "shh");
            }
            _ => panic!("expected Mint"),
        }
    }

    #[test]
    fn resolve_join_mode_uses_custom_secret_env_name() {
        let config = cfg(None, None, Some("client-123"), Some("MY_SECRET"));
        let mode = resolve_join_mode(
            &config,
            None,
            |name| (name == "MY_SECRET").then(|| "shh".to_string()),
            None,
        );
        assert!(matches!(mode, JoinMode::Mint { .. }));
    }

    #[test]
    fn resolve_join_mode_static_env_when_only_auth_key_env_resolves() {
        let config = cfg(None, Some("MY_KEY"), None, None);
        let mode = resolve_join_mode(
            &config,
            None,
            |name| (name == "MY_KEY").then(|| "tskey-auth-static".to_string()),
            None,
        );
        match mode {
            JoinMode::StaticEnv { key_env } => assert_eq!(key_env, "MY_KEY"),
            _ => panic!("expected StaticEnv"),
        }
    }

    #[test]
    fn resolve_join_mode_skips_when_neither_resolves() {
        let config = cfg(None, None, None, None);
        let mode = resolve_join_mode(&config, None, |_| None, None);
        assert!(matches!(mode, JoinMode::Skip));
    }

    #[test]
    fn resolve_join_mode_oauth_takes_precedence_over_static_env() {
        let config = cfg(None, Some("MY_KEY"), Some("client-123"), None);
        let mode = resolve_join_mode(
            &config,
            None,
            |name| match name {
                "MY_KEY" => Some("tskey-auth-static".to_string()),
                n if n == DEFAULT_OAUTH_SECRET_ENV => Some("shh".to_string()),
                _ => None,
            },
            None,
        );
        assert!(matches!(mode, JoinMode::Mint { .. }));
    }

    #[test]
    fn resolve_join_mode_falls_back_to_static_env_when_oauth_secret_empty() {
        let config = cfg(None, Some("MY_KEY"), Some("client-123"), None);
        let mode = resolve_join_mode(
            &config,
            None,
            |name| match name {
                "MY_KEY" => Some("tskey-auth-static".to_string()),
                n if n == DEFAULT_OAUTH_SECRET_ENV => Some(String::new()),
                _ => None,
            },
            None,
        );
        assert!(matches!(mode, JoinMode::StaticEnv { .. }));
    }

    #[test]
    fn resolve_join_mode_mints_from_stored_credential() {
        let config = cfg(None, None, None, None);
        let mode = resolve_join_mode(&config, None, |_| None, Some(("k-stored", "shh")));
        match mode {
            JoinMode::Mint {
                client_id,
                client_secret,
                ..
            } => {
                assert_eq!(client_id, "k-stored");
                assert_eq!(client_secret, "shh");
            }
            _ => panic!("expected Mint"),
        }
    }

    #[test]
    fn resolve_join_mode_config_env_beats_stored() {
        let config = cfg(None, None, Some("client-123"), None);
        let mode = resolve_join_mode(
            &config,
            None,
            |name| (name == DEFAULT_OAUTH_SECRET_ENV).then(|| "env-secret".to_string()),
            Some(("k-stored", "stored-secret")),
        );
        match mode {
            JoinMode::Mint {
                client_id,
                client_secret,
                ..
            } => {
                assert_eq!(client_id, "client-123");
                assert_eq!(client_secret, "env-secret");
            }
            _ => panic!("expected Mint"),
        }
    }

    #[test]
    fn resolve_join_mode_stored_beats_static_env() {
        let config = cfg(None, Some("MY_KEY"), None, None);
        let mode = resolve_join_mode(
            &config,
            None,
            |name| (name == "MY_KEY").then(|| "tskey-auth-static".to_string()),
            Some(("k-stored", "shh")),
        );
        assert!(matches!(mode, JoinMode::Mint { .. }));
    }

    #[test]
    fn resolve_join_mode_ignores_empty_stored_secret() {
        let config = cfg(None, None, None, None);
        let mode = resolve_join_mode(&config, None, |_| None, Some(("k-stored", "")));
        assert!(matches!(mode, JoinMode::Skip));
    }

    #[test]
    fn create_key_body_has_expected_shape() {
        let body = create_key_body("tag:lilbox-vm", "mybox", 300);
        assert_eq!(body["capabilities"]["devices"]["create"]["reusable"], false);
        assert_eq!(body["capabilities"]["devices"]["create"]["ephemeral"], true);
        assert_eq!(
            body["capabilities"]["devices"]["create"]["preauthorized"],
            true
        );
        assert_eq!(
            body["capabilities"]["devices"]["create"]["tags"],
            serde_json::json!(["tag:lilbox-vm"])
        );
        assert_eq!(body["expirySeconds"], 300);
        assert_eq!(body["description"], "lilbox mybox");
    }

    #[test]
    fn oauth_token_form_contains_grant_type_id_and_secret() {
        let form = oauth_token_form("my-id", "my-secret");
        assert!(form.contains(&("client_id", "my-id".to_string())));
        assert!(form.contains(&("client_secret", "my-secret".to_string())));
        assert!(form.contains(&("grant_type", "client_credentials".to_string())));
    }

    #[test]
    fn parse_access_token_reads_field() {
        let json = r#"{"access_token":"abc123","token_type":"bearer"}"#;
        assert_eq!(parse_access_token(json).unwrap(), "abc123");
    }

    #[test]
    fn parse_access_token_errs_on_missing_field() {
        assert!(parse_access_token(r#"{"token_type":"bearer"}"#).is_err());
    }

    #[test]
    fn parse_minted_key_reads_field() {
        let json = r#"{"id":"k1","key":"tskey-auth-minted","created":"now"}"#;
        assert_eq!(parse_minted_key(json).unwrap(), "tskey-auth-minted");
    }

    #[test]
    fn parse_minted_key_errs_on_missing_field() {
        assert!(parse_minted_key(r#"{"id":"k1"}"#).is_err());
    }

    #[test]
    fn join_failure_detail_no_tailscaled_names_lilbox_box() {
        let detail = join_failure_detail(3, "", "python");
        assert!(detail.contains("lilbox-box"));
        assert!(detail.contains("python"));
    }

    #[test]
    fn join_failure_detail_boot_failure_names_tailscaled() {
        let detail = join_failure_detail(4, "", "lilbox-box");
        assert!(detail.contains("tailscaled"));
    }

    #[test]
    fn join_failure_detail_prefers_stderr_over_canned_message() {
        for code in [3, 4, 5, 1] {
            assert_eq!(
                join_failure_detail(code, "  tailscale up: some real error  ", "python"),
                "tailscale up: some real error"
            );
        }
    }
}
