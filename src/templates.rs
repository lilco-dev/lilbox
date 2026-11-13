use anyhow::{Result, bail};

use crate::model::Template;

/// Canonical list of shipped starters, in display order. The `templates`
/// listing and the builtin-resolution test both iterate this — a template
/// added to `builtin_template` without an entry here is invisible in the UI.
pub(crate) const BUILTIN_NAMES: &[&str] = &[
    "node-dev",
    "python-dev",
    "rust-dev",
    "go-dev",
    "data-science",
    "ml-pytorch",
    "fullstack-web",
    "base-debian",
    "agent-sandbox",
    "desktop-vm",
];

pub(crate) fn builtin_templates() -> Vec<Template> {
    BUILTIN_NAMES
        .iter()
        .filter_map(|n| builtin_template(n))
        .collect()
}

pub(crate) fn builtin_template(name: &str) -> Option<Template> {
    let (manifest, setup) = match name {
        "python-dev" => (
            include_str!("../templates/python-dev/template.json"),
            include_str!("../templates/python-dev/setup.sh"),
        ),
        "node-dev" => (
            include_str!("../templates/node-dev/template.json"),
            include_str!("../templates/node-dev/setup.sh"),
        ),
        "rust-dev" => (
            include_str!("../templates/rust-dev/template.json"),
            include_str!("../templates/rust-dev/setup.sh"),
        ),
        "go-dev" => (
            include_str!("../templates/go-dev/template.json"),
            include_str!("../templates/go-dev/setup.sh"),
        ),
        "data-science" => (
            include_str!("../templates/data-science/template.json"),
            include_str!("../templates/data-science/setup.sh"),
        ),
        "ml-pytorch" => (
            include_str!("../templates/ml-pytorch/template.json"),
            include_str!("../templates/ml-pytorch/setup.sh"),
        ),
        "fullstack-web" => (
            include_str!("../templates/fullstack-web/template.json"),
            include_str!("../templates/fullstack-web/setup.sh"),
        ),
        "base-debian" => (
            include_str!("../templates/base-debian/template.json"),
            include_str!("../templates/base-debian/setup.sh"),
        ),
        "agent-sandbox" => (
            include_str!("../templates/agent-sandbox/template.json"),
            include_str!("../templates/agent-sandbox/setup.sh"),
        ),
        "desktop-vm" => (
            include_str!("../templates/desktop-vm/template.json"),
            include_str!("../templates/desktop-vm/setup.sh"),
        ),
        _ => return None,
    };
    Some(Template {
        name: name.into(),
        source: "builtin",
        dir: None,
        manifest: serde_json::from_str(manifest).expect("valid built-in template"),
        setup: Some(setup.into()),
        dockerfile: false,
    })
}

pub(crate) fn validate_template_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        bail!("invalid template name '{name}'");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_embedded() {
        let python = builtin_template("python-dev").unwrap();
        assert_eq!(python.manifest.image.as_deref(), Some("python"));
        assert!(python.setup.unwrap().contains("uv"));

        let rust = builtin_template("rust-dev").unwrap();
        assert_eq!(
            rust.manifest.image.as_deref(),
            Some("docker.io/library/rust:1-bookworm")
        );
        assert!(rust.setup.unwrap().contains("clippy"));

        let go = builtin_template("go-dev").unwrap();
        assert_eq!(
            go.manifest.image.as_deref(),
            Some("docker.io/library/golang:1.27-bookworm")
        );
        assert!(go.setup.unwrap().contains("gopls"));

        // Every shipped starter resolves and carries an image (guards a typo'd
        // include_str! path or a malformed template.json in any builtin).
        for name in BUILTIN_NAMES {
            let t = builtin_template(name).unwrap_or_else(|| panic!("missing builtin '{name}'"));
            assert!(t.manifest.image.is_some(), "builtin '{name}' has no image");
        }
    }

    #[test]
    fn rejects_template_traversal() {
        assert!(validate_template_name("../secret").is_err());
        assert!(validate_template_name("normal-name").is_ok());
    }
}
