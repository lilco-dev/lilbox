use std::{collections::HashMap, fs};

use anyhow::{Result, bail};
use microsandbox::{Image, Sandbox, Volume};

use crate::app::App;
use crate::cli::ImageCommand;
use crate::overlay;
use crate::sandbox::{status_name, statuses};
use crate::tailscale::tailnet_host;
use crate::templates::builtin_templates;
use crate::util::human_bytes;

pub(crate) async fn ls(app: &App, json: bool) -> Result<()> {
    let live = if json {
        statuses().await?
    } else {
        statuses().await.unwrap_or_default()
    };
    let rows = app.rows()?;
    if json {
        let values: Vec<_> = rows
            .into_iter()
            .map(|row| {
                let status = live
                    .get(&row.name)
                    .copied()
                    .map(status_name)
                    .unwrap_or_else(|| "gone".into());
                let tailnet_url = row.display_url();
                serde_json::json!({
                    "name": row.name,
                    "image": row.image,
                    "status": status,
                    "guest_port": row.guest_port,
                    "host_port": row.host_port,
                    "serve_port": row.serve_port,
                    "public": row.public,
                    "url": row.url,
                    "tailnet_url": tailnet_url,
                    "created": row.created,
                })
            })
            .collect();
        println!("{}", serde_json::to_string(&values)?);
    } else if rows.is_empty() {
        println!("no boxes yet - create one with: lilbox new");
    } else {
        println!("{:<20}  {:<20}  {:<10}  URL", "NAME", "IMAGE", "STATUS");
        for row in rows {
            let status = live
                .get(&row.name)
                .copied()
                .map(status_name)
                .unwrap_or_else(|| "gone".into());
            let display_url = row.display_url();
            println!(
                "{:<20}  {:<20}  {:<10}  {}",
                row.name,
                row.image,
                status,
                display_url.as_deref().unwrap_or("-")
            );
        }
    }
    Ok(())
}

pub(crate) fn templates(app: &App) -> Result<()> {
    let mut templates = builtin_templates();
    let user = app.templates_dir();
    if let Ok(entries) = fs::read_dir(user) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Ok(t) = app.template(&name) {
                templates.retain(|old| old.name != name);
                templates.push(t);
            }
        }
    }
    templates.sort_by(|a, b| a.name.cmp(&b.name));
    println!(
        "{:<18}  {:<22}  {:<7}  DESCRIPTION",
        "NAME", "IMAGE", "SOURCE"
    );
    for t in templates {
        println!(
            "{:<18}  {:<22}  {:<7}  {}",
            t.name,
            t.manifest.image.as_deref().unwrap_or("?"),
            t.source,
            t.manifest.description
        );
    }
    Ok(())
}

pub(crate) async fn volumes(app: &App) -> Result<()> {
    let owner: HashMap<_, _> = app
        .rows()?
        .into_iter()
        .filter_map(|r| r.volume.map(|v| (v, r.name)))
        .collect();
    let volumes = Volume::list().await?;
    println!("{:<28}  {:<20}  USED", "VOLUME", "BOX");
    for volume in volumes
        .into_iter()
        .filter(|v| v.name().starts_with("lilbox-") || owner.contains_key(v.name()))
    {
        println!(
            "{:<28}  {:<20}  {}",
            volume.name(),
            owner
                .get(volume.name())
                .map(String::as_str)
                .unwrap_or("orphan"),
            human_bytes(volume.used_bytes())
        );
    }
    Ok(())
}

pub(crate) async fn image(command: ImageCommand) -> Result<()> {
    match command {
        ImageCommand::Load { archive, tag } => {
            Image::load(&archive, vec![tag.clone()]).await?;
            println!("loaded {} as {tag}", archive.display());
        }
        ImageCommand::Ls => {
            println!("{:<40}  SIZE", "IMAGE");
            for image in Image::list().await? {
                println!(
                    "{:<40}  {}",
                    image.reference(),
                    image
                        .size_bytes()
                        .map(|n| human_bytes(n.max(0) as u64))
                        .unwrap_or_else(|| "-".into())
                );
            }
        }
        ImageCommand::Overlay { base, tag } => {
            let extra_files = vec![("etc/lilbox-overlay".to_string(), b"v1\n".to_vec())];
            overlay::overlay_image(&base, &extra_files, &tag).await?;
            println!("overlaid {base} as {tag}");
        }
        ImageCommand::Tailscalify { base, tag } => {
            overlay::tailscalify_image(&base, &tag).await?;
            println!("tailscalified {base} as {tag}");
        }
    }
    Ok(())
}

pub(crate) async fn stat(app: &App, name: String) -> Result<()> {
    let row = app.require_row(&name)?;
    let handle = Sandbox::get(&name).await?;
    println!(
        "name: {}\nstatus: {}\ntailscale node: {}\nconfig: {}",
        handle.name(),
        status_name(handle.status_snapshot()),
        row.tailscale_node.as_deref().unwrap_or("-"),
        serde_json::to_string_pretty(&serde_json::from_str::<serde_json::Value>(
            handle.config_json()
        )?)?
    );
    Ok(())
}

pub(crate) fn doctor(app: &App) -> Result<()> {
    println!(
        "lilbox doctor\n  runtime:    embedded microsandbox 0.6.8\n  tailscale:  {}\n  state db:   {}\n  tailnet:    {}\n",
        app.tailscale
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not found".into()),
        app.db_path().display(),
        app.tailscale
            .as_deref()
            .and_then(tailnet_host)
            .unwrap_or_else(|| "unknown".into())
    );
    let diagnosis = microsandbox::setup::diagnose();
    for section in &diagnosis.sections {
        println!("{}", section.title);
        for check in &section.checks {
            println!("  {:<22} {:?}: {}", check.label, check.state, check.value);
        }
    }
    if !diagnosis.is_healthy() {
        bail!("microsandbox host checks failed");
    }
    Ok(())
}
