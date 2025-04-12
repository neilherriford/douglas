use clap::{Arg, Command};
use unix_domain_socket_rest_client::http_executor::HttpClientBuilder;
use unix_domain_socket_rest_client::io_builder::IoBuilder;
use unix_domain_socket_rest_client::request_builder::LocalhostRequestBuilder;
use unix_domain_socket_rest_client::rest_client::Response;
use unix_domain_socket_rest_client::rest_client::RestClient;
use unix_domain_socket_rest_client::rest_client::SimpleRestClient;
use unix_domain_socket_rest_client::unix_domain_socket_io_builder::UnixDomainSocketIoBuilder;

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
        Some((cmd_bootstrap, sub_matches)) => {
            if sub_matches.get_flag("start") {
                let io_stream =
                    UnixDomainSocketIoBuilder::new(String::from("/var/run/docker.sock"))
                        .build()
                        .await
                        .unwrap();

                let mut client = SimpleRestClient::build(
                    Box::new(HttpClientBuilder::new()),
                    io_stream,
                    Box::new(LocalhostRequestBuilder::new()),
                )
                .await
                .unwrap();

                let outcome = client.get(String::from("/images/json")).await.unwrap();

                match outcome {
                    Response::Okay(Some(body)) => println!("{}", body),
                    _ => println!("oops"),
                };
            }
        }
        _ => todo!(),
    }
}
