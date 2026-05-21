#[cfg(feature = "mock")]
use mockall::predicate;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
#[cfg(feature = "mock")]
use std::os::unix::process::ExitStatusExt;

use std::{
    env::VarError,
    path::PathBuf,
    process::{Command, Output},
    time::Duration,
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum OsError {
    #[error("Invalid PID")]
    PidTooLarge,
    #[error("No such PID {0}")]
    NoSuchPid(u32),
    #[error("Insufficient access to kill pid {0}")]
    InsufficientAccessToKillPid(u32),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Process '{name}' returned a non success status")]
    ProccessExitStatusError {
        name: String,
        code: Option<i32>,
        args: Vec<String>,
    },
    #[error("Encountered error #{0}")]
    ErrorNumber(i32),
}

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait Os: Send + Sync {
    fn current_executable(&self) -> Result<PathBuf, OsError>;
    fn execute(
        &self,
        command: &str,
        args: Vec<String>,
        env: Vec<(String, String)>,
    ) -> Result<(), OsError>;
    fn execute_with_output(
        &self,
        command: &str,
        args: Vec<String>,
        env: Vec<(String, String)>,
    ) -> Result<Output, OsError>;
    fn spawn(
        &self,
        command: &str,
        args: Vec<String>,
        env: Vec<(String, String)>,
    ) -> Result<u32, OsError>;
    fn is_active_pid(&self, pid: u32) -> Result<bool, OsError>;
    fn kill(&self, pid: u32) -> Result<(), OsError>;
    fn exit(&self, code: i32);
    fn sleep(&self, duration: Duration);
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
        args: Vec<&str>,
        env: Vec<(&str, &str)>,
    ) -> &mut Self {
        self.expect_execute_with_output_for(command, args, env, "", "", 0);
        self
    }

    pub fn expect_execute_with_output_for(
        &mut self,
        command: &str,
        args: Vec<&str>,
        env: Vec<(&str, &str)>,
        stdout: &str,
        stderr: &str,
        status_code: i32,
    ) -> &mut Self {
        let expected_command = command.to_string();
        let expected_args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        let expected_env: Vec<(String, String)> = env
            .iter()
            .map(|pair| (pair.0.to_string(), pair.0.to_string()))
            .collect();
        let given_output = self.make_fake_output(stdout, stderr, status_code);

        self.expect_execute_with_output()
            .withf(move |given_command, given_args, given_env| {
                given_command == expected_command
                    && *given_args == expected_args
                    && *given_env == expected_env
            })
            .times(1)
            .return_once(move |_, _, _| Ok(given_output));
        self
    }

    pub fn expect_exit_with(&mut self, code: i32) -> &mut Self {
        self.expect_exit()
            .with(predicate::eq(code))
            .once()
            .returning(|_| panic!("mock exit"));

        self
    }

    pub fn given_pid_is_active(&mut self, pid: u32) -> &mut Self {
        self.expect_is_active_pid()
            .with(predicate::eq(pid))
            .once()
            .returning(|_| Ok(true));

        self
    }

    pub fn given_pid_is_not_active(&mut self, pid: u32) -> &mut Self {
        self.expect_is_active_pid()
            .with(predicate::eq(pid))
            .once()
            .returning(|_| Ok(false));

        self
    }
}

#[derive(Clone, Default)]
pub struct Unix {}

impl Unix {
    pub fn new() -> Self {
        Self {}
    }
}

impl Os for Unix {
    fn execute(
        &self,
        command: &str,
        args: Vec<String>,
        env: Vec<(String, String)>,
    ) -> Result<(), OsError> {
        self.execute_with_output(command, args, env)?;
        Ok(())
    }

    fn execute_with_output(
        &self,
        command: &str,
        args: Vec<String>,
        env: Vec<(String, String)>,
    ) -> Result<Output, OsError> {
        let output = Command::new(command).args(&args).envs(env).output()?;

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

    fn is_active_pid(&self, pid: u32) -> Result<bool, OsError> {
        let pid_i32 = i32::try_from(pid).map_err(|_| OsError::PidTooLarge)?;

        match kill(Pid::from_raw(pid_i32), None) {
            Ok(_) => Ok(true),
            Err(nix::errno::Errno::ESRCH) => Ok(false), // No such process
            Err(nix::errno::Errno::EPERM) => Ok(true),  // Process exists, but we lack permission
            Err(errno) => Err(OsError::ErrorNumber(errno as i32)),
        }
    }

    fn current_executable(&self) -> Result<PathBuf, OsError> {
        Ok(std::env::current_exe()?)
    }

    fn spawn(
        &self,
        command: &str,
        args: Vec<String>,
        env: Vec<(String, String)>,
    ) -> Result<u32, OsError> {
        Ok(Command::new(command).args(&args).envs(env).spawn()?.id())
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }

    fn kill(&self, pid: u32) -> Result<(), OsError> {
        let pid_i32 = i32::try_from(pid).map_err(|_| OsError::PidTooLarge)?;

        match kill(Pid::from_raw(pid_i32), Signal::SIGKILL) {
            Ok(_) => Ok(()),
            Err(nix::errno::Errno::ESRCH) => Err(OsError::NoSuchPid(pid)), // No such process
            Err(nix::errno::Errno::EPERM) => Err(OsError::InsufficientAccessToKillPid(pid)), // Process exists, but we lack permission
            Err(errno) => Err(OsError::ErrorNumber(errno as i32)),
        }
    }
}

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait EnvironmentVariableReader {
    fn read(&self, name: &str) -> Result<String, VarError>;
}

pub struct UnixEnvironmentVariableReader {}

impl UnixEnvironmentVariableReader {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for UnixEnvironmentVariableReader {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvironmentVariableReader for UnixEnvironmentVariableReader {
    fn read(&self, name: &str) -> Result<String, VarError> {
        std::env::var(name)
    }
}
