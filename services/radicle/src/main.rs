use clap::{Arg, Command};
use docker::SimpleDockerClient;
use docker::container::Repository;
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

                let result = client.find_by_name("authentik".to_string()).await;

                match result {
                    Ok(obj) => {
                        println!("✅{:?}", obj);
                    }
                    Err(err) => println!("🚨{:?}", err),
                }
            }
        }
        _ => todo!(),
    }
}

fn container_pretty_print(container: Container) {
    println!("Container {{");
    println!("  id:                      {}", container.id);
    println!("  name:                    {}", container.name);
    println!("  status:                  {}", container.status);
    println!("  image: {{");
    println!(
        "    id: {}:{}",
        container.image.id.algorithm, container.image.id.hex
    );
    println!("    tags: [");
    for tag in container.image.tags {
        println!("      {}:{}", tag.name, tag.version);
    }
    println!("    ]");
    println!("  }}");
    println!("  mounts   [");
    for mount in container.mounts {
        println!("    {{",);
        println!("      type:     {}", mount.mount_type);
        println!("      source:   {}", mount.source);
        println!("      dest:     {}", mount.destination);
        println!("      writable: {}", mount.writable);
        println!("    }}",);
    }
    println!("  ]");
    println!("  networks [");
    for network in container.networks {
        println!("    {{");
        println!("      id:     {}", network.id);
        println!("      name:   {}", network.name);
        println!("      labels: [");
        for label in network.labels {
            println!("        {}: {}", label.name, label.value);
        }
        println!("      ]");
        println!("    }}");
    }
    println!("  ]");
    println!("  environmenment variables [");
    for environment_variable in container.environment_variables {
        println!(
            "    {}={}",
            environment_variable.name, environment_variable.value
        );
    }
    println!("  ]");
    println!("  labels: [");
    for label in container.labels {
        println!("    {}: {}", label.name, label.value);
    }
    println!("  ]");
    println!("}}");
}
