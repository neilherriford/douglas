use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(ValueEnum, Clone, Debug, Copy)]
pub(crate) enum OutputStyle {
    Plain,
    Json,
}

#[derive(ValueEnum, Clone, Debug)]
pub(crate) enum Switch {
    Enabled,
    Disabled,
}

#[derive(ValueEnum, Clone, Debug, Copy)]
pub(crate) enum KickTarget {
    Bract,
    Resin,
    Seedbank,
}

impl std::fmt::Display for KickTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KickTarget::Bract => f.write_str(config::services::BRACT),
            KickTarget::Resin => f.write_str(config::services::RESIN),
            KickTarget::Seedbank => f.write_str(config::services::SEEDBANK),
        }
    }
}

impl KickTarget {
    pub(crate) fn service_name(self) -> &'static str {
        match self {
            KickTarget::Bract => config::services::BRACT,
            KickTarget::Resin => config::services::RESIN,
            KickTarget::Seedbank => config::services::SEEDBANK,
        }
    }
}

#[derive(Parser, Debug)]
#[command(about = "Initialize and run the secure core of Douglas")]
#[command(version = env!("CARGO_PKG_VERSION"))]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,

    #[arg(
        long,
        global = true,
        value_enum,
        help = "Set the output mode. Defaults to plain text for most commands; `start` defaults \
                to an interactive TUI instead and only switches to plain/json output when this \
                is explicitly set, since plain/json mode has no live terminal to render into."
    )]
    pub(crate) output_style: Option<OutputStyle>,
    #[arg(
        long,
        global = true,
        value_enum,
        default_value_t = Switch::Enabled,
        help = "When enabled, intermediate messages are displayed, hidden otherwise."
    )]
    pub(crate) verbose: Switch,
}

#[derive(Subcommand, Debug)]
#[command(name = "douglas", about = "My awesome CLI", version = "1.0")]
pub(crate) enum Commands {
    #[command(about = "Start Douglas")]
    Start {
        #[arg(long, default_value_t = false, help = "Only display the start plan")]
        plan_only: bool,
    },
    #[command(about = "Stop Douglas")]
    Stop {
        #[arg(long, default_value_t = false, help = "Only display the start plan")]
        plan_only: bool,
    },
    #[command(about = "Report the status of seedlings and services")]
    Status,
    #[command(hide = true)]
    Service {
        #[command(subcommand)]
        service: ServiceCommand,
    },
    #[command(hide = true)]
    Kick {
        #[arg(value_enum)]
        name: KickTarget,
    },
    #[command(about = "Seedling commands")]
    Seedling {
        #[command(subcommand)]
        seedling: SeedlingCommand,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum ServiceCommand {
    Bract {
        #[arg(long, help = "File descriptor to stream boot information to")]
        notify_fd: i32,
    },
    Resin {
        #[arg(
            long,
            default_value_t = false,
            help = "Debug mode — runs in foreground with TUI, no pipe required"
        )]
        dbg: bool,
        #[arg(
            long,
            help = "File descriptor to stream boot information to",
            required_unless_present = "dbg"
        )]
        notify_fd: Option<i32>,
    },
    Seedbank {
        #[arg(
            long,
            default_value_t = false,
            help = "Debug mode — runs in foreground with TUI, no pipe required"
        )]
        dbg: bool,
        #[arg(
            long,
            help = "File descriptor to stream boot information to",
            required_unless_present = "dbg"
        )]
        notify_fd: Option<i32>,
    },
    Woodward {
        #[arg(
            long,
            default_value_t = false,
            help = "Debug mode — runs in foreground with TUI"
        )]
        dbg: bool,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum SeedlingCommand {
    #[command(about = "Query seedling status")]
    Status {
        #[arg(long, help = "The seedling to to query")]
        name: String,
    },
    #[command(about = "Stop a running seedling")]
    Stop {
        #[arg(long, help = "The seedling to to stop")]
        name: String,
    },
    #[command(about = "Delete a stopped seedling")]
    Drop {
        #[arg(long, help = "The seedling to to drop")]
        name: String,
    },
    #[command(about = "Start a seedling")]
    Start {
        #[arg(long, help = "The seedling to to start")]
        name: String,
    },
    #[command(about = "Create a new seedling")]
    New {
        #[arg(long, help = "The seedling to to create")]
        name: String,
        #[arg(
            long,
            help = "Path to a TOML seedling spec file; reads stdin if omitted"
        )]
        file: Option<PathBuf>,
    },
    #[command(about = "Create a blank template for a seedling")]
    CreateTemplate,
    #[command(
        about = "Find and remove deadwood containers, networks, route files, and resin repositories"
    )]
    Prune {
        #[arg(
            long,
            default_value_t = false,
            help = "Skip the confirmation prompt and prune immediately"
        )]
        yes: bool,
    },
}

impl std::fmt::Display for Commands {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Commands::Start { .. } => f.write_str("start"),
            Commands::Stop { .. } => f.write_str("stop"),
            Commands::Status => f.write_str("status"),
            Commands::Service {
                service: ServiceCommand::Bract { .. },
            } => f.write_str("service bract"),
            Commands::Service {
                service: ServiceCommand::Resin { .. },
            } => f.write_str("service resin"),
            Commands::Service {
                service: ServiceCommand::Seedbank { .. },
            } => f.write_str("service seedbank"),
            Commands::Seedling {
                seedling: SeedlingCommand::Status { .. },
            } => f.write_str("seedling status"),
            Commands::Seedling {
                seedling: SeedlingCommand::Start { .. },
            } => f.write_str("start seedling"),
            Commands::Seedling {
                seedling: SeedlingCommand::Stop { .. },
            } => f.write_str("stop seedling"),
            Commands::Seedling {
                seedling: SeedlingCommand::Drop { .. },
            } => f.write_str("drop seedling"),
            Commands::Seedling {
                seedling: SeedlingCommand::New { .. },
            } => f.write_str("create seedling"),
            Commands::Seedling {
                seedling: SeedlingCommand::CreateTemplate,
            } => f.write_str("create seedling template"),
            Commands::Seedling {
                seedling: SeedlingCommand::Prune { .. },
            } => f.write_str("prune deadwood"),
            Commands::Service {
                service: ServiceCommand::Woodward { .. },
            } => f.write_str("service woodward"),
            Commands::Kick { name } => write!(f, "kick {}", name.service_name()),
        }
    }
}

#[cfg(test)]
mod command_coverage_tests {
    use super::Cli;
    use clap::CommandFactory;
    use std::path::PathBuf;

    fn collect_leaf_command_paths(command: &clap::Command, prefix: &str) -> Vec<String> {
        let mut paths = Vec::new();
        for sub in command.get_subcommands() {
            if sub.is_hide_set() {
                continue;
            }
            let path = if prefix.is_empty() {
                sub.get_name().to_string()
            } else {
                format!("{prefix} {}", sub.get_name())
            };
            if sub.has_subcommands() {
                paths.extend(collect_leaf_command_paths(sub, &path));
            } else {
                paths.push(path);
            }
        }
        paths
    }

    fn steps_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testing-utils/smoke-tests/steps")
    }

    fn covered_command_paths() -> Vec<String> {
        let dir = steps_dir();
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|err| panic!("should read {}: {err}", dir.display()));

        let mut covered = Vec::new();
        for entry in entries {
            let Ok(entry) = entry else {
                panic!("should read directory entry");
            };
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("sh") {
                continue;
            }
            let contents = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("should read {}: {err}", path.display()));
            for line in contents.lines() {
                if let Some(command) = line.trim().strip_prefix("# covers:") {
                    covered.push(command.trim().to_string());
                }
            }
        }
        covered
    }

    #[test]
    fn test_smoke_test_steps_should_cover_every_non_hidden_cli_command() {
        let mut actual = collect_leaf_command_paths(&Cli::command(), "");
        actual.sort();
        actual.dedup();

        let mut expected = covered_command_paths();
        expected.sort();
        expected.dedup();

        assert_eq!(
            actual, expected,
            "testing-utils/smoke-tests/steps/*.sh is out of sync with the CLI's actual command \
             surface (left = live CLI commands, right = commands claimed via `# covers:` \
             lines) — add, rename, or remove a step's `# covers:` line to match"
        );
    }
}

#[cfg(test)]
mod kick_target_tests {
    use super::KickTarget;

    #[test]
    fn test_service_name_should_map_each_target_to_its_service_name() {
        assert_eq!(KickTarget::Bract.service_name(), "bract");
        assert_eq!(KickTarget::Resin.service_name(), "resin");
        assert_eq!(KickTarget::Seedbank.service_name(), "seedbank");
    }

    #[test]
    fn test_display_should_match_the_service_name() {
        assert_eq!(KickTarget::Bract.to_string(), "bract");
        assert_eq!(KickTarget::Resin.to_string(), "resin");
        assert_eq!(KickTarget::Seedbank.to_string(), "seedbank");
    }
}
