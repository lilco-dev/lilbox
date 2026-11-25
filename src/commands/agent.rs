use std::path::PathBuf;
use std::{env, fs, process::Command};

use anyhow::{Context, Result, anyhow, bail};

use crate::app::App;
use crate::cli::{AgentArgs, NewArgs};
use crate::sandbox::{connect_box, run_guest};
use crate::tailscale::is_valid_env_var_name;
use crate::util::{find_program, random_name};

use super::new::{NewOptions, cmd_new};

pub(crate) const WORKSPACE: &str = "/workspace";

fn parse_exec_env(pairs: &[String]) -> Result<Vec<(String, String)>> {
    pairs
        .iter()
        .map(|pair| {
            let (key, value) = pair.split_once('=').context("--env expects KEY=VAL")?;
            if !is_valid_env_var_name(key) {
                bail!("--env key '{key}' is not a valid name ([A-Za-z_][A-Za-z0-9_]*)");
            }
            Ok((key.to_owned(), value.to_owned()))
        })
        .collect()
}

fn workspace_path(app: &App, workspace: Option<PathBuf>, clone: Option<&str>) -> Result<PathBuf> {
    let path = if let Some(url) = clone {
        let repo = url.trim_end_matches('/').rsplit('/').next().unwrap_or("");
        let repo = repo.strip_suffix(".git").unwrap_or(repo);
        if repo.is_empty() || matches!(repo, "." | "..") {
            bail!("could not determine workspace name from clone URL");
        }
        let root = app.workspaces_dir();
        fs::create_dir_all(&root)?;
        let dest = root.join(repo);
        if dest.exists() {
            bail!("clone target already exists: {}", dest.display());
        }
        let git = find_program("git").ok_or_else(|| anyhow!("git not found"))?;
        let status = Command::new(git)
            .args(["clone", "--depth", "1", "--", url])
            .arg(&dest)
            .status()?;
        if !status.success() {
            bail!("git clone failed");
        }
        dest
    } else {
        workspace.map(Ok).unwrap_or_else(env::current_dir)?
    };
    if !path.is_dir() {
        bail!("workspace not found: {}", path.display());
    }
    path.canonicalize()
        .with_context(|| format!("resolve workspace {}", path.display()))
}

pub(crate) async fn cmd_agent(app: &App, args: AgentArgs) -> Result<i32> {
    let exec_env = parse_exec_env(&args.env)?;
    let name = match args.name {
        Some(name) => name,
        None => random_name(app)?,
    };
    if app.row(&name)?.is_some() {
        bail!("box '{name}' already exists");
    }
    let workspace = workspace_path(app, args.workspace, args.clone.as_deref())?;
    if let Some(source) = args.agents_file {
        let target = workspace.join("AGENTS.md");
        if source.canonicalize()? != target.canonicalize().unwrap_or_else(|_| target.clone()) {
            fs::copy(source, target)?;
        }
    }
    cmd_new(
        app,
        NewArgs {
            name: Some(name.clone()),
            template: Some("exeuntu".into()),
            image: args.image,
            ..Default::default()
        },
        NewOptions {
            workspace: Some(workspace),
            ..Default::default()
        },
    )
    .await?;
    if args.prepare {
        return Ok(0);
    }

    let sandbox = connect_box(app, &name, false).await?;
    let mut command = vec![args.agent];
    command.extend(args.agent_args);
    run_guest(&sandbox, &command, Some(WORKSPACE), &exec_env).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_env_preserves_equals_in_values_and_rejects_bad_keys() {
        assert_eq!(
            parse_exec_env(&["MY_VALUE=a=b".into()]).unwrap(),
            vec![("MY_VALUE".into(), "a=b".into())]
        );
        assert!(parse_exec_env(&["1BAD=value".into()]).is_err());
        assert!(parse_exec_env(&["NO_EQUALS".into()]).is_err());
    }
}
