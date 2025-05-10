use clap::{Arg, Command};
use docker::{DockerImageRepository, SimpleDockerClient};
use simple_rest_client::log::StdOutLogger;
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
                let logger = StdOutLogger::new();

                let mut client =
                    SimpleDockerClient::build("/var/run/docker.sock".to_string(), Arc::new(logger))
                        .await
                        .expect("it worked");

                let result = client
                    .pull(
                        "hello-world",
                        docker::Version::Specific("linux".to_string()),
                    )
                    .await;

                match result {
                    Ok(image) => println!("{:?}", image),
                    Err(err) => println!("🚨{}", err.to_string()),
                }
            }
        }
        _ => todo!(),
    }
}
