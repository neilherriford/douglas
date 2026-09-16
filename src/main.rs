mod bootstrap;
#[macro_use]
pub(crate) mod macros;
mod cli;
mod cli_reporter;
mod commands;
mod daemon;
mod services;
mod util;
mod verify;

use crate::cli::{Cli, Commands, OutputStyle, SeedlingCommand, ServiceCommand};
use crate::commands::stop::stop;
use crate::commands::{
    kick::kick, prune::prune_deadwood, seedling, start::start, status::status, verify::verify,
};
use crate::daemon::run_with_tokio;
use crate::services::{
    resin_debug_mode, seedbank_debug_mode, start_bract, start_resin, start_seedbank,
    start_woodward, woodward_debug_mode,
};
use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let output_style_arg = cli.output_style;
    let output_style = output_style_arg.unwrap_or(OutputStyle::Plain);

    match cli.command {
        Commands::Start { plan_only } => run_with_tokio(start(plan_only, output_style_arg)),
        Commands::Stop { plan_only } => run_with_tokio(stop(plan_only, output_style_arg)),
        Commands::Status => run_with_tokio(status(output_style)),
        Commands::Service {
            service: ServiceCommand::Bract { notify_fd },
        } => start_bract(notify_fd),
        Commands::Service {
            service: ServiceCommand::Resin { dbg: true, .. },
        } => run_with_tokio(resin_debug_mode()),
        Commands::Service {
            service:
                ServiceCommand::Resin {
                    notify_fd: Some(fd),
                    ..
                },
        } => start_resin(fd),
        Commands::Service {
            service: ServiceCommand::Seedbank { dbg: true, .. },
        } => run_with_tokio(seedbank_debug_mode()),
        Commands::Service {
            service:
                ServiceCommand::Seedbank {
                    notify_fd: Some(fd),
                    ..
                },
        } => start_seedbank(fd),
        Commands::Service {
            service: ServiceCommand::Resin { .. } | ServiceCommand::Seedbank { .. },
        } => unreachable!("clap requires --notify-fd when --dbg is not set"),
        Commands::Service {
            service: ServiceCommand::Woodward { dbg: true },
        } => run_with_tokio(woodward_debug_mode()),
        Commands::Service {
            service: ServiceCommand::Woodward { dbg: false },
        } => start_woodward(),
        Commands::Kick { name } => run_with_tokio(kick(name)),
        Commands::Seedling {
            seedling: SeedlingCommand::Status { name },
        } => run_with_tokio(seedling::get_seedling_status(&name, output_style)),
        Commands::Seedling {
            seedling: SeedlingCommand::Start { name },
        } => run_with_tokio(seedling::run_seedling_action(
            &name,
            output_style,
            seedling::SeedlingAction::Start,
        )),
        Commands::Seedling {
            seedling: SeedlingCommand::Stop { name },
        } => run_with_tokio(seedling::run_seedling_action(
            &name,
            output_style,
            seedling::SeedlingAction::Stop,
        )),
        Commands::Seedling {
            seedling: SeedlingCommand::Drop { name },
        } => run_with_tokio(seedling::run_seedling_action(
            &name,
            output_style,
            seedling::SeedlingAction::Drop,
        )),
        Commands::Seedling {
            seedling: SeedlingCommand::New { name, file },
        } => run_with_tokio(seedling::create_seedling(
            &name,
            file.as_deref(),
            output_style,
        )),
        Commands::Seedling {
            seedling: SeedlingCommand::CreateTemplate,
        } => seedling::create_seedling_template(output_style),
        Commands::Seedling {
            seedling: SeedlingCommand::Prune { yes },
        } => run_with_tokio(prune_deadwood(output_style, yes)),
        Commands::Verify { path } => verify(path),
    }
}
