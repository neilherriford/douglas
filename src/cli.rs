use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(ValueEnum, Clone, Debug, Copy)]
pub(crate) enum OutputStyle {
    Plain,
    Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Presentation {
    Interactive,
    Plain,
    Json,
}

impl Presentation {
    pub(crate) fn resolve(output_style: Option<OutputStyle>) -> Self {
        match output_style {
            None => Presentation::Interactive,
            Some(OutputStyle::Plain) => Presentation::Plain,
            Some(OutputStyle::Json) => Presentation::Json,
        }
    }

    pub(crate) fn output_style(self) -> OutputStyle {
        match self {
            Presentation::Interactive | Presentation::Plain => OutputStyle::Plain,
            Presentation::Json => OutputStyle::Json,
        }
    }

    pub(crate) fn console_style(self) -> Option<OutputStyle> {
        match self {
            Presentation::Interactive => None,
            Presentation::Plain => Some(OutputStyle::Plain),
            Presentation::Json => Some(OutputStyle::Json),
        }
    }
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
        help = "Set the output mode. Defaults to plain text for most commands; `start`, `stop` and \
                `upgrade` default to an interactive TUI instead and only switch to plain/json \
                output when this is explicitly set, since plain/json mode has no live terminal \
                to render into."
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
        #[arg(long, default_value_t = false, help = "Only display the stop plan")]
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
    #[command(about = "Verify a candidate douglas binary's signature")]
    Verify {
        #[arg(
            long,
            help = "Path to a candidate binary to verify; defaults to the currently running douglas binary"
        )]
        path: Option<PathBuf>,
    },
    #[command(about = "Upgrade the douglas system")]
    Upgrade {
        #[arg(long, default_value_t = false, help = "Only display the upgrade plan")]
        plan_only: bool,
        #[arg(
            long,
            default_value_t = false,
            help = "Proceed even when the new version cannot be rolled back from"
        )]
        allow_one_way: bool,
        #[arg(long, help = "Path to the new version")]
        path: PathBuf,
    },
    #[command(about = "Roll back to a version kept from an earlier upgrade")]
    Rollback {
        #[arg(long, default_value_t = false, help = "Only display the rollback plan")]
        plan_only: bool,
        #[arg(
            long,
            help = "Version to roll back to; defaults to the newest kept version older than the running one"
        )]
        to: Option<String>,
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

#[derive(ValueEnum, Clone, Debug, Copy, Default)]
pub(crate) enum TemplateStyle {
    #[default]
    LocalImage,
    ForeignImage,
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
    CreateTemplate {
        #[arg(long, value_enum, default_value_t = TemplateStyle::default())]
        template_style: TemplateStyle,
        #[arg(
            long,
            help = "Mount the seedling at the root level as opposed to a subdomain.  Only one seedling may be at the root"
        )]
        root: bool,
    },
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
                seedling: SeedlingCommand::CreateTemplate { .. },
            } => f.write_str("create seedling template"),
            Commands::Seedling {
                seedling: SeedlingCommand::Prune { .. },
            } => f.write_str("prune deadwood"),
            Commands::Service {
                service: ServiceCommand::Woodward { .. },
            } => f.write_str("service woodward"),
            Commands::Kick { name } => write!(f, "kick {}", name.service_name()),
            Commands::Verify { .. } => f.write_str("verify"),
            Commands::Upgrade { .. } => f.write_str("upgrade"),
            Commands::Rollback { .. } => f.write_str("rollback"),
        }
    }
}

#[cfg(test)]
mod presentation_tests {
    use super::{OutputStyle, Presentation};

    #[test]
    fn test_resolve_should_treat_a_missing_style_as_interactive() {
        assert_eq!(Presentation::resolve(None), Presentation::Interactive);
    }

    #[test]
    fn test_resolve_should_map_explicit_styles() {
        assert_eq!(
            Presentation::resolve(Some(OutputStyle::Plain)),
            Presentation::Plain
        );
        assert_eq!(
            Presentation::resolve(Some(OutputStyle::Json)),
            Presentation::Json
        );
    }

    #[test]
    fn test_output_style_should_fall_back_to_plain_when_interactive() {
        assert!(matches!(
            Presentation::Interactive.output_style(),
            OutputStyle::Plain
        ));
        assert!(matches!(
            Presentation::Json.output_style(),
            OutputStyle::Json
        ));
    }

    #[test]
    fn test_console_style_should_be_absent_only_when_interactive() {
        assert!(Presentation::Interactive.console_style().is_none());
        assert!(matches!(
            Presentation::Plain.console_style(),
            Some(OutputStyle::Plain)
        ));
        assert!(matches!(
            Presentation::Json.console_style(),
            Some(OutputStyle::Json)
        ));
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

    fn smoke_tests_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testing-utils/smoke-tests")
    }

    fn collect_script_paths(dir: &std::path::Path, scripts: &mut Vec<PathBuf>) {
        let entries = std::fs::read_dir(dir)
            .unwrap_or_else(|err| panic!("should read {}: {err}", dir.display()));
        for entry in entries {
            let Ok(entry) = entry else {
                panic!("should read directory entry");
            };
            let path = entry.path();
            if path.is_dir() {
                collect_script_paths(&path, scripts);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("sh") {
                scripts.push(path);
            }
        }
    }

    fn covered_command_paths() -> Vec<String> {
        let mut scripts = Vec::new();
        collect_script_paths(&smoke_tests_dir().join("setup"), &mut scripts);
        collect_script_paths(&smoke_tests_dir().join("scenarios"), &mut scripts);

        let mut covered = Vec::new();
        for path in scripts {
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
    fn test_smoke_test_scripts_should_cover_every_non_hidden_cli_command() {
        let mut actual = collect_leaf_command_paths(&Cli::command(), "");
        actual.sort();
        actual.dedup();

        let mut expected = covered_command_paths();
        expected.sort();
        expected.dedup();

        assert_eq!(
            actual, expected,
            "the scripts under testing-utils/smoke-tests/setup and scenarios are out of sync with the CLI's actual command \
             surface (left = live CLI commands, right = commands claimed via `# covers:` \
             lines) — add, rename, or remove a step's `# covers:` line to match"
        );
    }
}

#[cfg(test)]
mod create_template_argument_tests {
    use super::{Cli, Commands, SeedlingCommand, TemplateStyle};
    use clap::{Parser, error::ErrorKind};

    fn parse(arguments: &[&str]) -> Result<Cli, clap::Error> {
        let mut command_line = vec!["douglas", "seedling", "create-template"];
        command_line.extend_from_slice(arguments);
        Cli::try_parse_from(command_line)
    }

    fn create_template_arguments(arguments: &[&str]) -> (TemplateStyle, bool) {
        let Ok(cli) = parse(arguments) else {
            panic!("should parse {arguments:?}");
        };
        let Commands::Seedling {
            seedling:
                SeedlingCommand::CreateTemplate {
                    template_style,
                    root,
                },
        } = cli.command
        else {
            panic!("should parse as create-template");
        };

        (template_style, root)
    }

    #[test]
    fn test_create_template_should_default_to_the_local_image_style_without_root() {
        let (template_style, root) = create_template_arguments(&[]);

        assert!(matches!(template_style, TemplateStyle::LocalImage));
        assert!(!root);
    }

    #[test]
    fn test_create_template_should_accept_the_local_image_style_explicitly() {
        let (template_style, root) = create_template_arguments(&["--template-style=local-image"]);

        assert!(matches!(template_style, TemplateStyle::LocalImage));
        assert!(!root);
    }

    #[test]
    fn test_create_template_should_accept_the_foreign_image_style() {
        let (template_style, root) = create_template_arguments(&["--template-style=foreign-image"]);

        assert!(matches!(template_style, TemplateStyle::ForeignImage));
        assert!(!root);
    }

    #[test]
    fn test_create_template_should_accept_the_root_flag_alone() {
        let (template_style, root) = create_template_arguments(&["--root"]);

        assert!(matches!(template_style, TemplateStyle::LocalImage));
        assert!(root);
    }

    #[test]
    fn test_create_template_should_accept_a_style_followed_by_the_root_flag() {
        let (template_style, root) =
            create_template_arguments(&["--template-style=foreign-image", "--root"]);

        assert!(matches!(template_style, TemplateStyle::ForeignImage));
        assert!(root);
    }

    #[test]
    fn test_create_template_should_accept_the_root_flag_followed_by_a_style() {
        let (template_style, root) =
            create_template_arguments(&["--root", "--template-style=foreign-image"]);

        assert!(matches!(template_style, TemplateStyle::ForeignImage));
        assert!(root);
    }

    #[test]
    fn test_create_template_should_reject_an_unknown_style() {
        let Err(error) = parse(&["--template-style=bogus"]) else {
            panic!("should reject an unknown style");
        };

        assert_eq!(error.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn test_create_template_should_display_as_create_seedling_template_whatever_the_flags() {
        let Ok(cli) = parse(&["--root", "--template-style=foreign-image"]) else {
            panic!("should parse");
        };

        assert_eq!(cli.command.to_string(), "create seedling template");
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
