use file_system::FileSystemError;
use mockall::automock;
use serde::Serialize;
use serde_json::{Value, json};
use std::path::Path;

use crate::status_command::{BractStatus, DouglasStatus};

#[automock]
pub trait CommandOutputPrinter<TOutput: 'static, TError> {
    fn print(&self, command: &str, result: &Result<TOutput, TError>)
    where
        TError: std::fmt::Display + 'static;
}

#[derive(Default)]
pub struct PlainCommandOutputPrinter {}

impl PlainCommandOutputPrinter {
    pub fn new() -> Self {
        Self {}
    }

    fn path_to_string(path: &Path) -> String {
        if let Some(result) = path.to_str() {
            result.to_string()
        } else {
            "<Unable to determine path>".to_string()
        }
    }

    fn print_indented(indent: u8, text: &str) {
        let mut indentation = String::new();
        for _ in 0..indent {
            indentation += "  ";
        }
        println!("{indentation}{text}");
    }

    fn print_ok(command: &str) {
        println!("{command}: OK!");
    }

    fn print_error<T>(command: &str, error: &T)
    where
        T: std::fmt::Display,
    {
        eprintln!("❌ {command}: error: {error}");
    }

    fn print_command(command: &str) {
        println!("{command}:");
    }
}

impl CommandOutputPrinter<DouglasStatus, FileSystemError> for PlainCommandOutputPrinter {
    fn print(&self, command: &str, result: &Result<DouglasStatus, FileSystemError>) {
        Self::print_command(command);
        match result {
            Ok(status) => {
                Self::print_indented(1, "Bract:");
                match &status.bract_status {
                    BractStatus::CannotDetermineStatus(message) => {
                        Self::print_indented(2, &format!("Cannot determine status: {message}"));
                    }
                    BractStatus::NotIntialized => Self::print_indented(2, "Not intialized."),
                    BractStatus::NotRunning => Self::print_indented(2, "Not running"),
                    BractStatus::Status(bract_status) => {
                        Self::print_indented(
                            2,
                            &format!(
                                "Mount path: {}",
                                Self::path_to_string(bract_status.mount_root.as_path())
                            ),
                        );
                        Self::print_indented(
                            2,
                            &format!(
                                "Token path: {}",
                                Self::path_to_string(bract_status.token_path.as_path())
                            ),
                        );
                        Self::print_indented(2, "Services:");
                        for service in &bract_status.services {
                            Self::print_indented(3, &service.name);
                            Self::print_indented(4, "Mounts:");

                            for mount in &service.mounts {
                                Self::print_indented(
                                    5,
                                    &format!("{}: {}", mount.name, mount.version),
                                );
                            }
                        }
                    }
                }
                Self::print_indented(1, "Docker:");
                match &status.docker_status {
                    crate::status_command::DockerStatus::Active => {
                        Self::print_indented(2, "Active");
                    }
                    crate::status_command::DockerStatus::ConfigFileNotFound => {
                        Self::print_indented(
                            2,
                            "Could not find config file, has the system been initialized yet?",
                        );
                    }
                    crate::status_command::DockerStatus::DockerClientError(message)
                    | crate::status_command::DockerStatus::CouldNotLoadConfiguration(message) => {
                        Self::print_indented(2, &format!("Docker error: {message}"));
                    }
                }
            }
            Err(err) => Self::print_error(command, err),
        }
    }
}

impl<T: std::fmt::Display> CommandOutputPrinter<(), T> for PlainCommandOutputPrinter {
    fn print(&self, command: &str, result: &Result<(), T>) {
        match result {
            Ok(()) => Self::print_ok(command),
            Err(err) => Self::print_error(command, err),
        }
    }
}

#[derive(Default)]
pub struct JsonCommandOutputPrinter {}

impl JsonCommandOutputPrinter {
    pub fn new() -> Self {
        Self {}
    }

    fn print_success(command: &str, data: &Value) {
        Self::print_json(command, data, &Value::Null);
    }

    fn print_failure(command: &str, error: &Value) {
        Self::print_json(command, &Value::Null, error);
    }

    fn print_error<T>(command: &str, error: &T)
    where
        T: std::fmt::Display,
    {
        Self::print_failure(command, &Value::String(format!("{error}")));
    }

    fn print_ok(command: &str) {
        Self::print_success(command, &Value::String("OK".to_string()));
    }

    fn print_json(command: &str, data: &Value, error: &Value) {
        let json = json!({
            "command": command,
            "data": data,
            "error": error,
        });

        match serde_json::to_string_pretty(&json) {
            Ok(text) => {
                if error == &Value::Null {
                    eprintln!("{text}");
                } else {
                    println!("{text}");
                }
            }
            Err(err) => eprintln!(
                "{{\n  \"command\": \"{command}\",\n  \"data\": null,\n  \"error\": \"{err:?}\"\n}}",
            ),
        }
    }

    fn serialize<T>(value: &T, name: &str) -> Value
    where
        T: Serialize,
    {
        match serde_json::to_value(value) {
            Ok(status) => status,
            Err(err) => {
                eprintln!("Error serializing bract {name}: {err}",);
                json!({"status": "data_error"})
            }
        }
    }
}

impl CommandOutputPrinter<DouglasStatus, FileSystemError> for JsonCommandOutputPrinter {
    fn print(&self, command: &str, result: &Result<DouglasStatus, FileSystemError>) {
        match result {
            Ok(status) => {
                let bract_status = match &status.bract_status {
                    BractStatus::NotIntialized => {
                        json!({"status": "not_initialized"})
                    }
                    BractStatus::NotRunning => {
                        json!({"status": "not_running"})
                    }
                    BractStatus::Status(status) => Self::serialize(&status, "bract status"),
                    BractStatus::CannotDetermineStatus(message) => {
                        json!({"status": "unknown", "details": message})
                    }
                };
                let docker_status = match &status.docker_status {
                    crate::status_command::DockerStatus::Active => json!({"status": "active"}),
                    crate::status_command::DockerStatus::ConfigFileNotFound => {
                        json!({"status": "config_file_not_found"})
                    }
                    crate::status_command::DockerStatus::DockerClientError(message) => {
                        json!({"status": "docker_client_error", "details": message})
                    }
                    crate::status_command::DockerStatus::CouldNotLoadConfiguration(message) => {
                        json!({"status": "could_not_load_configuration", "details": message})
                    }
                };

                let data = json!({
                    "bract": bract_status,
                    "docker": docker_status,
                });
                Self::print_success(command, &data);
            }
            Err(err) => Self::print_error(command, err),
        }
    }
}

impl<T: std::fmt::Display> CommandOutputPrinter<(), T> for JsonCommandOutputPrinter {
    fn print(&self, command: &str, result: &Result<(), T>) {
        match result {
            Ok(()) => Self::print_ok(command),
            Err(err) => Self::print_error(command, err),
        }
    }
}
