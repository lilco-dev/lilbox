mod agent;
mod cp;
pub(crate) mod gateway;
mod lifecycle;
mod net;
mod new;
mod template;
mod view;

use anyhow::Result;

use crate::app::App;
use crate::cli::Command;
use crate::logs::cmd_logs;
use crate::sandbox;

pub(crate) async fn dispatch(app: &App, command: Command) -> Result<i32> {
    match command {
        Command::New(args) => {
            let attach = args.attach;
            return new::cmd_new(
                app,
                args,
                new::NewOptions {
                    attach,
                    ..new::NewOptions::default()
                },
            )
            .await;
        }
        Command::Ls { json } => view::ls(app, json).await?,
        Command::Gc => lifecycle::gc(app).await?,
        Command::Templates => view::templates(app)?,
        Command::Template(args) => template::cmd_template(app, args.command)?,
        Command::Provision { name } => lifecycle::provision_cmd(app, name).await?,
        Command::Exec(args) => return sandbox::exec(app, args).await,
        Command::Ssh(args) => return sandbox::ssh(app, args).await,
        Command::Cp { src, dst } => cp::cp(app, src, dst).await?,
        Command::Logs(args) => cmd_logs(app, args).await?,
        Command::Run(args) => return sandbox::run(app, args).await,
        Command::Expose { name, public } => net::expose(app, name, public)?,
        Command::Unexpose { name } => net::unexpose(app, name)?,
        Command::Join { name, tailnet_tag } => net::join(app, name, tailnet_tag).await?,
        Command::Leave { name } => net::leave(app, name).await?,
        Command::Stop { name } => lifecycle::stop(app, name).await?,
        Command::Start { name } => lifecycle::start(app, name).await?,
        Command::Restart { name } => lifecycle::restart(app, name).await?,
        Command::Rm { name, keep_data } => lifecycle::rm(app, name, keep_data).await?,
        Command::Fork {
            name,
            newname,
            force,
        } => lifecycle::fork(app, name, newname, force).await?,
        Command::Rebuild { name, image } => lifecycle::rebuild(app, name, image).await?,
        Command::Volumes => view::volumes(app).await?,
        Command::Image(args) => view::image(args.command).await?,
        Command::Stat { name } => view::stat(app, name).await?,
        Command::Url { name } => net::url(app, name)?,
        Command::Agent(args) => return agent::cmd_agent(app, args).await,
        Command::Gateway(args) => return gateway::cmd_gateway(app, args).await,
        Command::Doctor => view::doctor(app)?,
        // Handled in main() before App::new() so completion generation never
        // initializes user state; do not move this into dispatch.
        Command::Completions { .. } => {
            unreachable!("completions is handled in main before App init")
        }
    }
    Ok(0)
}
