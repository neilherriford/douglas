use mockall::predicate;
#[cfg(feature = "mock")]
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Output};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum OsError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Proccess '{name}' returned a non success status")]
    ProccessExitStatusError {
        name: String,
        code: Option<i32>,
        args: Vec<String>,
    },
}

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait Os: Send + Sync {
    fn execute(&self, command: &str, args: Vec<String>) -> Result<(), OsError>;
    fn execute_with_output(&self, command: &str, args: Vec<String>) -> Result<Output, OsError>;

    fn exit(&self, code: i32);
}

#[cfg(feature = "mock")]
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
    ) -> &mut Self {
        self.expect_execute_with_output_for(command, args, "", "", 0);
        self
    }

    pub fn expect_execute_with_output_for(
        &mut self,
        command: &str,
        args: Vec<String>,
        stdout: &str,
        stderr: &str,
        status_code: i32,
    ) -> &mut Self {
        let expected_command = command.to_string();
        let expected_args = args.clone();
        let given_output = self.make_fake_output(stdout, stderr, status_code);

        self.expect_execute_with_output()
            .withf(move |given_command, given_args| {
                given_command == expected_command && *given_args == expected_args
            })
            .times(1)
            .return_once(move |_, _| Ok(given_output));
        self
    }

    pub fn expect_exit_with(&mut self, code: i32) -> &mut Self {
        self.expect_exit()
            .with(predicate::eq(code))
            .once()
            .returning(|_| panic!("mock exit"));

        self
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
                args,
            })
        }
    }

    fn exit(&self, code: i32) {
        std::process::exit(code);
    }
}
