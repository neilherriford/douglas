use mockall::automock;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Output};
use thiserror::Error;
use users::{get_group_by_name, get_user_by_name};

#[derive(Error, Debug)]
pub enum OsError {
    #[error("The name could not be converted to unicode")]
    InvalidName,
    #[error("Could not find user '{name}'")]
    UserNotFoundError { name: String },
    #[error("Could not find group '{name}'")]
    GroupNotFoundError { name: String },
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Proccess '{name}' returned a non success status")]
    ProccessExitStatusError {
        name: String,
        code: Option<i32>,
        args: Vec<String>,
    },
}

#[automock]
pub trait Os: Send + Sync {
    fn is_root(&self) -> bool;
    fn user_exists(&self, name: &str) -> bool;

    fn group_exists(&self, name: &str) -> bool;
    fn group_memberships(&self, name: &str) -> Vec<String>;
    fn get_group_id(&self, name: &str) -> Option<u32>;

    fn execute(&self, command: &str, args: Vec<String>) -> Result<(), OsError>;
    fn execute_with_output(&self, command: &str, args: Vec<String>) -> Result<Output, OsError>;

    fn exit(&self, code: i32);
}

impl MockOs {
    fn make_fake_output(&self, stdout: &str, stderr: &str, status_code: i32) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(status_code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }
    pub fn expect_execute_with_output_with_empty_success(
        &mut self,
        command: &str,
        args: Vec<String>,
    ) {
        self.expect_execute_with_output_for(command, args, "", "", 0);
    }

    pub fn expect_execute_with_output_for(
        &mut self,
        command: &str,
        args: Vec<String>,
        stdout: &str,
        stderr: &str,
        status_code: i32,
    ) {
        let expected_command = command.to_string();
        let expected_args = args.clone();
        let given_output = self.make_fake_output(stdout, stderr, status_code);

        self.expect_execute_with_output()
            .withf(move |given_command, given_args| {
                given_command == expected_command && *given_args == expected_args
            })
            .times(1)
            .return_once(move |_, _| Ok(given_output));
    }
}

#[derive(Clone)]
pub struct Unix {}

impl Unix {
    pub fn new() -> Self {
        Self {}
    }
}

impl Os for Unix {
    fn is_root(&self) -> bool {
        nix::unistd::Uid::effective().is_root()
    }

    fn user_exists(&self, name: &str) -> bool {
        get_user_by_name(name).is_some()
    }
    fn group_exists(&self, name: &str) -> bool {
        get_group_by_name(name).is_some()
    }
    fn group_memberships(&self, name: &str) -> Vec<String> {
        if let Some(user) = get_user_by_name(name) {
            if let Some(groups) = user.groups() {
                return groups
                    .iter()
                    .filter_map(|group| group.name().to_str())
                    .map(|group_name| group_name.to_string())
                    .collect();
            }
        }

        vec![]
    }

    fn get_group_id(&self, name: &str) -> Option<u32> {
        if let Some(group) = get_group_by_name(name) {
            Some(group.gid())
        } else {
            None
        }
    }

    fn execute(&self, command: &str, args: Vec<String>) -> Result<(), OsError> {
        self.execute_with_output(command, args)?;
        Ok(())
    }

    fn execute_with_output(&self, command: &str, args: Vec<String>) -> Result<Output, OsError> {
        let output = Command::new(command).args(&args).output()?;

        if output.status.success() {
            Ok(output)
        } else {
            Err(OsError::ProccessExitStatusError {
                name: command.to_string(),
                code: output.status.code(),
                args: args,
            })
        }
    }

    fn exit(&self, code: i32) {
        std::process::exit(code);
    }
}
