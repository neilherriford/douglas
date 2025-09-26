use crate::status_command::DouglasStatus;
use file_system::FileSystemError;
use mockall::automock;
use serde::Serialize;
use serde_json::{Value, json};
use std::path::Path;

#[automock]
pub trait CommandOutputPrinter<TOutput: 'static, TError> {
    fn print(&self, command: &str, result: &Result<TOutput, TError>)
    where
        TError: std::fmt::Display + 'static;
}

pub struct PlainCommandOutputPrinter {}

impl PlainCommandOutputPrinter {
    pub fn new() -> Self {
        Self {}
    }

    fn path_to_string(&self, path: &Path) -> String {
        if let Some(result) = path.to_str() {
            result.to_string()
        } else {
            "<Unable to determine path>".to_string()
        }
    }

    fn print_indented(&self, indent: u8, text: &str) {
        let mut indentation = String::new();
        for _ in 0..indent {
            indentation += "  ";
        }
        println!("{}{}", indentation, text);
    }

    fn print_ok(&self, command: &str) {
        println!("{command}: OK!");
    }

    fn print_error<T>(&self, command: &str, error: &T)
    where
        T: std::fmt::Display,
    {
        eprintln!("❌ {command}: error: {error}");
    }

    fn print_command(&self, command: &str) {
        println!("{command}:");
    }
}

impl CommandOutputPrinter<DouglasStatus, FileSystemError> for PlainCommandOutputPrinter {
    fn print(&self, command: &str, result: &Result<DouglasStatus, FileSystemError>) {
        self.print_command(command);
        match result {
            Ok(status) => match &status.bract_status {
                crate::status_command::BractStatus::CannotDetermineStatus => {
                    self.print_indented(1, "Cannot determine status.")
                }
                crate::status_command::BractStatus::NotIntialized => {
                    self.print_indented(1, "Not intialized.")
                }
                crate::status_command::BractStatus::NotRunning => {
                    self.print_indented(1, "Not running")
                }
                crate::status_command::BractStatus::Status(bract_status) => {
                    self.print_indented(
                        2,
                        &format!(
                            "Mount path: {}",
                            self.path_to_string(bract_status.mount_root.as_path())
                        ),
                    );
                    self.print_indented(
                        2,
                        &format!(
                            "Token path: {}",
                            self.path_to_string(bract_status.token_path.as_path())
                        ),
                    );
                    self.print_indented(2, "Services:");
                    for service in &bract_status.services {
                        self.print_indented(3, &service.name);
                        self.print_indented(4, "Mounts:");

                        for mount in &service.mounts {
                            self.print_indented(5, &format!("{}: {}", mount.name, mount.version));
                        }
                    }
                }
            },
            Err(err) => self.print_error(command, err),
        }
    }
}

impl<T: std::fmt::Display> CommandOutputPrinter<(), T> for PlainCommandOutputPrinter {
    fn print(&self, command: &str, result: &Result<(), T>) {
        match result {
            Ok(_) => self.print_ok(command),
            Err(err) => self.print_error(command, err),
        }
    }
}

pub struct JsonCommandOutputPrinter {}

impl JsonCommandOutputPrinter {
    pub fn new() -> Self {
        Self {}
    }

    fn print_success(&self, command: &str, data: Value) {
        self.print_json(command, data, Value::Null);
    }

    fn print_failure(&self, command: &str, error: Value) {
        self.print_json(command, Value::Null, error);
    }

    fn print_error<T>(&self, command: &str, error: &T)
    where
        T: std::fmt::Display,
    {
        self.print_failure(command, Value::String(format!("{error}")));
    }

    fn print_ok(&self, command: &str) {
        self.print_success(command, Value::String("OK".to_string()));
    }

    fn print_json(&self, command: &str, data: Value, error: Value) {
        let json = json!({
            "command": command,
            "data": data,
            "error": error,
        });

        match serde_json::to_string_pretty(&json) {
            Ok(text) => {
                if error == Value::Null {
                    eprintln!("{}", text)
                } else {
                    println!("{}", text)
                }
            }
            Err(err) => eprintln!(
                "{{\n  \"command\": \"{}\",\n  \"data\": null,\n  \"error\": \"{:?}\"\n}}",
                command, err
            ),
        }
    }

    fn serialize<T>(&self, value: &T, name: &str) -> Value
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
                    crate::status_command::BractStatus::NotIntialized => {
                        json!({"status": "not_initialized"})
                    }
                    crate::status_command::BractStatus::NotRunning => {
                        json!({"status": "not_running"})
                    }
                    crate::status_command::BractStatus::Status(status) => {
                        self.serialize(&status, "bract status")
                    }
                    crate::status_command::BractStatus::CannotDetermineStatus => {
                        json!({"status": "unknown"})
                    }
                };
                let data = json!({
                    "bract": bract_status
                });
                self.print_success(command, data);
            }
            Err(err) => self.print_error(command, err),
        }
    }
}

impl<T: std::fmt::Display> CommandOutputPrinter<(), T> for JsonCommandOutputPrinter {
    fn print(&self, command: &str, result: &Result<(), T>) {
        match result {
            Ok(_) => self.print_ok(command),
            Err(err) => self.print_error(command, err),
        }
    }
}
