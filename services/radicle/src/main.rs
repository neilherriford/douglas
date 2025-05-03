use clap::{Arg, Command};
use docker::{DockerClient, SimpleDockerClient};
use simple_rest_client::log::SilentLogger;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let cmd_bootstrap: &str = "bootstrap";

    let matches = Command::new("radicle")
        .version("0.01")
        .author("Neil Herriford")
        .about("Bootstrap for douglas")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .allow_external_subcommands(true)
        .subcommand(
            Command::new(cmd_bootstrap)
                .about("Bootstrap douglas")
                .arg(
                    Arg::new("start")
                        .long("start")
                        .help("Starts the bootstrap proccess")
                        .action(clap::ArgAction::SetTrue),
                )
                .arg_required_else_help(true),
        )
        .get_matches();

    match matches.subcommand() {
        Some((_cmd_bootstrap, sub_matches)) => {
            if sub_matches.get_flag("start") {
                let logger = SilentLogger::new();

                let mut client =
                    SimpleDockerClient::build("/var/run/docker.sock".to_string(), Arc::new(logger))
                        .await
                        .expect("it worked");

                let images = client.list_images().await.expect("uhoh");

                println!("{:?}", images);
            }
        }
        _ => todo!(),
    }
}
