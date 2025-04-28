use clap::{Arg, Command};
use simple_rest_client::log::StdOutLogger;
use simple_rest_client::unix_domain_socket::build_client as build_uds_client;
use simple_rest_client::{Request, Response, RestClient};
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
                    build_uds_client("/var/run/docker.sock".to_string(), Arc::new(logger))
                        .await
                        .expect("it worked");

                let req = Request::Get {
                    path: "/images/json".to_string(),
                    headers: None,
                };

                let response: Result<Response<String>, Box<dyn std::error::Error>> =
                    client.execute(&req).await;

                match response {
                    Ok(Response::Okay {
                        headers: _,
                        body: Some(body),
                    }) => println!("{}", body),
                    _ => println!("oops"),
                }
            }
        }
        _ => todo!(),
    }
}
