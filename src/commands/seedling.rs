use crate::cli::OutputStyle;
use crate::commands::{CommandContext, parse_seedling_name, print_error, print_status_json};
use ::config::DouglasFolders;
use bract_client::Client;
use crossterm::style::Stylize;
use file_system::{FileReader, FileSystemError, UnixFileReader};
use seedbank_types::{HealthCheck, HealthCheckCommand};
use std::{
    collections::{HashMap, HashSet},
    num::NonZeroU8,
    path::{Path, PathBuf},
    process::ExitCode,
    str::FromStr,
};

fn example_name(value: &str) -> seedbank_types::Name {
    value
        .parse()
        .unwrap_or_else(|_| unreachable!("'{value}' is a valid seedling name literal"))
}

fn example_user_seedling_definition() -> seedbank_types::UserSeedlingDefinition {
    let mut mounts = HashMap::new();

    mounts.insert(
        example_name("config"),
        seedbank_types::Mount::with_files(
            seedbank_types::MountType::Persisted,
            PathBuf::from("/etc/example/config"),
            seedbank_types::AccessMode::ReadOnly,
            HashSet::new(),
        ),
    );

    mounts.insert(
        example_name("cache"),
        seedbank_types::Mount::with_files(
            seedbank_types::MountType::InMemory,
            PathBuf::from("/var/cache/example"),
            seedbank_types::AccessMode::Writable,
            HashSet::new(),
        ),
    );

    mounts.insert(
        example_name("shared-assets"),
        seedbank_types::Mount::with_files(
            seedbank_types::MountType::PersistedShared(vec![
                example_name("sibling-a"),
                example_name("sibling-b"),
            ]),
            PathBuf::from("/var/lib/example/shared"),
            seedbank_types::AccessMode::Writable,
            HashSet::new(),
        ),
    );

    mounts.insert(
        example_name("log"),
        seedbank_types::Mount::with_files(
            seedbank_types::MountType::Persisted,
            PathBuf::from("/var/log/example"),
            seedbank_types::AccessMode::Writable,
            HashSet::new(),
        )
        .rotating_logs(),
    );

    seedbank_types::UserSeedlingDefinition::new(
        mounts,
        seedbank_types::PortSpec {
            public: 8080,
            additional: vec![seedbank_types::PortMapping {
                external: 1234,
                internal: 4321,
            }],
        },
        HealthCheck {
            #[allow(clippy::unwrap_used)]
            command: HealthCheckCommand::from_str("true").unwrap(),
            #[allow(clippy::unwrap_used)]
            wait_time_in_seconds: NonZeroU8::new(1).unwrap(),
        },
    )
}

pub(crate) fn create_seedling_template(output_style: OutputStyle) -> ExitCode {
    let toml = match toml::to_string_pretty(&example_user_seedling_definition()) {
        Ok(toml) => toml,
        Err(err) => {
            eprintln!("{}", format!("Could not render template: {err}").red());
            return ExitCode::from(1);
        }
    };

    match output_style {
        OutputStyle::Plain => println!("{toml}"),
        OutputStyle::Json => match serde_json::to_string(&serde_json::json!({ "toml": toml })) {
            Ok(json) => println!("{json}"),
            Err(err) => {
                eprintln!(
                    "{}",
                    format!("Could not serialize template as JSON: {err}").red()
                );
                return ExitCode::from(1);
            }
        },
    }

    ExitCode::from(0)
}

fn read_user_seedling_definition_input(
    file_reader: &dyn FileReader,
    file: Option<&Path>,
) -> Result<String, FileSystemError> {
    match file {
        Some(path) => file_reader.read_all(path),
        None => file_reader.read_stdin(),
    }
}

pub(crate) async fn create_seedling(
    name: &str,
    file: Option<&Path>,
    output_style: OutputStyle,
) -> ExitCode {
    let context = CommandContext::plain();
    let douglas_folders = &context.douglas_folders;
    let guard = context.task("Creating seedling");

    let Some(seedling_name) = parse_seedling_name(&guard, output_style, name) else {
        return ExitCode::from(1);
    };

    let client = bract_client::UdsClient::new(guard.reporter(), douglas_folders);

    let file_reader = UnixFileReader::new();

    let input = match read_user_seedling_definition_input(&file_reader, file) {
        Ok(input) => input,
        Err(err) => {
            print_error(
                output_style,
                &format!("Could not read seedling spec: {err}"),
            );
            return ExitCode::from(1);
        }
    };

    let user_seedling_definition: seedbank_types::UserSeedlingDefinition =
        match toml::from_str(&input) {
            Ok(user_seedling_definition) => user_seedling_definition,
            Err(err) => {
                print_error(output_style, &format!("Invalid seedling spec:\n\n{err}"));
                return ExitCode::from(1);
            }
        };

    match client
        .new_seedling(&seedling_name, &user_seedling_definition)
        .await
    {
        Ok(message) => {
            match output_style {
                OutputStyle::Plain => println!("{message}"),
                OutputStyle::Json => {
                    match serde_json::to_string(
                        &serde_json::json!({ "success": true, "message": message }),
                    ) {
                        Ok(json) => println!("{json}"),
                        Err(err) => {
                            print_error(
                                output_style,
                                &format!("Could not serialize response as JSON: {err}"),
                            );
                            return ExitCode::from(1);
                        }
                    }
                }
            }
            guard.finish_with_outcome(log::Outcome::Ok);
            ExitCode::from(0)
        }
        Err(err) => {
            let message = format!("Could not create seedling: {err}");
            guard.span().message(log::Level::Warn, &message);
            print_error(output_style, &err.to_string());
            ExitCode::from(1)
        }
    }
}

pub(crate) async fn get_seedling_status(name: &str, output_style: OutputStyle) -> ExitCode {
    let context = CommandContext::plain();
    let douglas_folders = &context.douglas_folders;
    let guard = context.task("Fetching status");

    let Some(seedling_name) = parse_seedling_name(&guard, output_style, name) else {
        return ExitCode::from(1);
    };

    let result = fetch_status(&guard, douglas_folders, output_style, seedling_name).await;

    if result == ExitCode::SUCCESS {
        guard.finish_with_outcome(log::Outcome::Ok);
    }

    result
}

pub(crate) async fn fetch_status(
    guard: &log::ScopeGuard,
    douglas_folders: &DouglasFolders,
    output_style: OutputStyle,
    seedling_name: seedbank_types::Name,
) -> ExitCode {
    let client = bract_client::UdsClient::new(guard.reporter(), douglas_folders);

    match client.seedling_status(&seedling_name).await {
        Ok(status) => match output_style {
            OutputStyle::Plain => println!("{status}"),
            OutputStyle::Json => {
                if let Err(err) = print_status_json(&status) {
                    let message = format!("Could not serialize status as JSON: {err}");
                    guard.span().message(log::Level::Warn, &message);
                    print_error(output_style, &err.to_string());
                    return ExitCode::from(1);
                }
            }
        },
        Err(err) => {
            let message = format!("Could not determine seedling status: {err}");
            guard.span().message(log::Level::Warn, &message);
            print_error(output_style, &err.to_string());
            return ExitCode::from(1);
        }
    }

    ExitCode::from(0)
}

pub(crate) enum SeedlingAction {
    Start,
    Stop,
    Drop,
}

impl SeedlingAction {
    fn label(&self) -> &'static str {
        match self {
            SeedlingAction::Start => "Starting seedling",
            SeedlingAction::Stop => "Stopping seedling",
            SeedlingAction::Drop => "Dropping seedling",
        }
    }

    fn error_prefix(&self) -> &'static str {
        match self {
            SeedlingAction::Start => "Could not start seedling",
            SeedlingAction::Stop => "Could not stop seedling",
            SeedlingAction::Drop => "Could not drop seedling",
        }
    }

    async fn invoke(
        &self,
        client: &dyn bract_client::Client,
        name: &seedbank_types::Name,
    ) -> Result<(), bract_client::Error> {
        match self {
            SeedlingAction::Start => client.start_seedling(name).await,
            SeedlingAction::Stop => client.stop_seedling(name).await,
            SeedlingAction::Drop => client.drop_seedling(name).await,
        }
    }
}

pub(crate) async fn run_seedling_action(
    name: &str,
    output_style: OutputStyle,
    action: SeedlingAction,
) -> ExitCode {
    let context = CommandContext::plain();
    let douglas_folders = &context.douglas_folders;
    let guard = context.task(action.label());

    let Some(seedling_name) = parse_seedling_name(&guard, output_style, name) else {
        return ExitCode::from(1);
    };

    let client = bract_client::UdsClient::new(guard.reporter(), douglas_folders);

    match action.invoke(&client, &seedling_name).await {
        Ok(()) => {
            let status_result =
                fetch_status(&guard, douglas_folders, output_style, seedling_name).await;

            if status_result != ExitCode::SUCCESS {
                return status_result;
            }
        }
        Err(err) => {
            let message = format!("{}: {err}", action.error_prefix());
            guard.span().message(log::Level::Warn, &message);
            print_error(output_style, &err.to_string());
            return ExitCode::from(1);
        }
    }

    guard.finish_with_outcome(log::Outcome::Ok);

    ExitCode::from(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use file_system::MockFileReader;

    #[test]
    fn test_example_user_seedling_definition_should_declare_the_expected_mounts() {
        let definition = example_user_seedling_definition();

        assert_eq!(definition.mounts.len(), 4);
        assert!(definition.mounts.contains_key(&example_name("config")));
        assert!(definition.mounts.contains_key(&example_name("cache")));
        assert!(
            definition
                .mounts
                .contains_key(&example_name("shared-assets"))
        );
        assert!(definition.mounts.contains_key(&example_name("log")));
    }

    #[test]
    fn test_example_user_seedling_definition_should_opt_the_log_mount_into_rotation() {
        let definition = example_user_seedling_definition();

        let Some(log_mount) = definition.mounts.get(&example_name("log")) else {
            panic!("should declare a log mount");
        };

        assert!(log_mount.rotate_logs());
    }

    #[test]
    fn test_hello_world_example_definition_should_opt_its_log_mount_into_rotation() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("example-seedlings/hello-world/default.toml");
        let Ok(contents) = std::fs::read_to_string(&path) else {
            panic!("should read {}", path.display());
        };
        let Ok(definition) = toml::from_str::<seedbank_types::UserSeedlingDefinition>(&contents)
        else {
            panic!("should parse hello-world's example seedling definition");
        };

        let Some(log_mount) = definition.mounts.get(&example_name("log")) else {
            panic!("should declare a log mount");
        };

        assert!(log_mount.rotate_logs());
    }

    #[test]
    fn test_read_user_seedling_definition_input_should_read_the_given_file_when_present() {
        let mut file_reader = MockFileReader::new();
        file_reader
            .expect_read_all()
            .withf(|path| path == Path::new("/tmp/spec.toml"))
            .returning(|_| Ok("name = \"example\"".to_string()));

        let Ok(result) =
            read_user_seedling_definition_input(&file_reader, Some(Path::new("/tmp/spec.toml")))
        else {
            panic!("should read the given file");
        };

        assert_eq!(result, "name = \"example\"");
    }

    #[test]
    fn test_read_user_seedling_definition_input_should_read_stdin_when_no_file_given() {
        let mut file_reader = MockFileReader::new();
        file_reader
            .expect_read_stdin()
            .returning(|| Ok("from stdin".to_string()));

        let Ok(result) = read_user_seedling_definition_input(&file_reader, None) else {
            panic!("should read stdin");
        };

        assert_eq!(result, "from stdin");
    }

    #[test]
    fn test_seedling_action_label_should_describe_each_action() {
        assert_eq!(SeedlingAction::Start.label(), "Starting seedling");
        assert_eq!(SeedlingAction::Stop.label(), "Stopping seedling");
        assert_eq!(SeedlingAction::Drop.label(), "Dropping seedling");
    }

    #[test]
    fn test_seedling_action_error_prefix_should_describe_each_action() {
        assert_eq!(
            SeedlingAction::Start.error_prefix(),
            "Could not start seedling"
        );
        assert_eq!(
            SeedlingAction::Stop.error_prefix(),
            "Could not stop seedling"
        );
        assert_eq!(
            SeedlingAction::Drop.error_prefix(),
            "Could not drop seedling"
        );
    }
}
