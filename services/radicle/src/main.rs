use clap::{Arg, Command};
// use docker::images;
use uds_utils;

// mod services;
// use services::action::Action;
// use services::docker::images::list::List;

#[tokio::main]
async fn main() {
    let cmd_bootstrap: &str = "bootstrap";
    // let arg_server: &str = "server";

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
                let client = uds_utils::Client::new(String::from("/var/run/docker.sock"));
                let response = client.get(String::from("/images/json")).await.unwrap();

                match response {
                    uds_utils::Response::Okay(Some(body)) => println!("{}", body),
                    _ => println!("oops"),
                };

                // let boffo = uds_utils::buffer(String::from("/var/run/docker.sock"), req).await;
                // let body = images::list().await.unwrap();
                // println!("{:?}", body);
                // let l = List;
                // let _ = l.fart().await;
            }
        }
        _ => todo!(),
    }

    // .arg(
    //     Arg::new(ARG_BOOTSTRAP)
    //         .long(ARG_BOOTSTRAP)
    //         .help("Starts the bootstrap proccess")
    //         .action(clap::ArgAction::SetTrue)
    //         .conflicts_with(arg_server),
    // )
    // .arg(
    //     Arg::new(arg_server)
    //         .long(ARG_SERVER)
    //         .help("Starts the admin server")
    //         .action(clap::ArgAction::SetTrue)
    //         .conflicts_with(ARG_BOOTSTRAP),
    // )
    // .get_matches();

    // if matches.get_flag("fart") {
    //     println!("--fart flag is set!");
    //     let l = List;
    //     let _ = l.fart().await;
    // } else {
    //     println!("--fart flag is not set.");
    // }
}
