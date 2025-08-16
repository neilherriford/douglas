use bract::Server;
use clap::{Arg, Command};

use file_system::{
    LocalFileDeleter, LocalFileReader, LocalFileWriter, LocalFolder, LocalLinks, LocalPermissions,
    LocalUnixDomainSocket,
};
use log::StdOutLogger;
use os::Unix;
use std::path::Path;
use std::sync::Arc;

fn main() {
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
                let result = Server::new(
                    Arc::new(StdOutLogger::new()),
                    Arc::new(LocalFileReader::new()),
                    Arc::new(LocalFileWriter::new()),
                    Arc::new(LocalFileDeleter::new()),
                    Arc::new(LocalFolder::new()),
                    Arc::new(LocalLinks::new()),
                    Arc::new(Unix::new()),
                    Arc::new(LocalPermissions::new()),
                    Arc::new(LocalUnixDomainSocket::new()),
                    Path::new("/tmp/doug.token"),
                    Path::new("/tmp/smelly.sock"),
                    Path::new("/tmp/mounts"),
                )
                .start();

                println!("survey sez: {:?}", result);

                // let result = Permissions::new(os, directory).start().await;
                // println!("{:?}", result);
            }
        }
        _ => todo!(),
    }
}

// fn container_pretty_print(container: Container) {
//     println!("Container {{");
//     println!("  id:                      {}", container.id);
//     println!("  name:                    {}", container.name);
//     println!("  status:                  {}", container.status);
//     println!("  image: {{");
//     println!(
//         "    id: {}:{}",
//         container.image.id.algorithm, container.image.id.hex
//     );
//     println!("    tags: [");
//     for tag in container.image.tags {
//         println!("      {}:{}", tag.name, tag.version);
//     }
//     println!("    ]");
//     println!("  }}");
//     println!("  mounts   [");
//     for mount in container.mounts {
//         println!("    {{",);
//         println!("      type:     {}", mount.mount_type);
//         println!("      source:   {}", mount.source);
//         println!("      dest:     {}", mount.destination);
//         println!("      writable: {}", mount.writable);
//         println!("    }}",);
//     }
//     println!("  ]");
//     println!("  environmenment variables [");
//     for environment_variable in container.environment_variables {
//         println!(
//             "    {}={}",
//             environment_variable.name, environment_variable.value
//         );
//     }
//     println!("  ]");
//     println!("  labels: [");
//     for label in container.labels {
//         println!("    {}: {}", label.name, label.value);
//     }
//     println!("  ]");
//     println!("}}");
// }
