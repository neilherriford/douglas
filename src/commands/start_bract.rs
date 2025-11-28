use crate::{
    Commands,
    douglas_flags_reader::{DouglasFlag, EnvironmentVariableDouglasFlagsReader},
};
use log::Logger;
use os::Os;
use std::{sync::Arc, time::Duration};

pub struct StartBract<'a> {
    log: &'a dyn Logger,
    client: &'a mut bract::Client,
    os: Arc<dyn Os>,
}

impl<'a> StartBract<'a> {
    pub fn new(log: &'a dyn Logger, client: &'a mut bract::Client, os: Arc<dyn Os>) -> Self {
        Self { log, client, os }
    }

    pub async fn perform(&mut self) -> bool {
        let _ = self.client.shutdown().await;

        let Some(pid) = self.start() else {
            return false;
        };

        self.os.sleep(Duration::from_secs(1));

        self.assert_pid_is_active(pid)
    }

    fn get_executable_path(&self) -> Option<String> {
        let current_executable_path = match self.os.current_executable() {
            Ok(path) => path,
            Err(err) => {
                self.log
                    .error(&format!("Failed to determine current executable: {err}"));
                return None;
            }
        };

        if let Some(string_path) = current_executable_path.to_str() {
            Some(string_path.into())
        } else {
            self.log
                .error("Current path is not representable in unicode");
            None
        }
    }

    fn start(&self) -> Option<u32> {
        let command = self.get_executable_path()?;

        let args = vec![Commands::Start { daemonize: false }.to_string()];
        let env = vec![(
            EnvironmentVariableDouglasFlagsReader::environment_variable_name(),
            EnvironmentVariableDouglasFlagsReader::serialize(&vec![DouglasFlag::BractOnly]),
        )];

        match self.os.spawn(&command, args, env) {
            Ok(pid) => {
                self.log.info(&format!("Bract daemon started (pid: {pid})"));
                Some(pid)
            }
            Err(err) => {
                self.log
                    .error(&format!("Could not start Bract daemon: '{err}'"));
                None
            }
        }
    }

    fn assert_pid_is_active(&self, pid: u32) -> bool {
        match self.os.is_active_pid(pid) {
            Ok(true) => true,
            Ok(false) => {
                self.log.error("Bract daemon stoped unexpectedly (pid: {})");
                false
            }
            Err(err) => {
                self.log
                    .error(&format!("Could not check Bract daemon status: '{err}'"));
                false
            }
        }
    }
}
