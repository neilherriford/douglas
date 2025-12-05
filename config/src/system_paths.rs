use crate::constants::APPLICATION_NAME;
use std::path::PathBuf;

pub trait SystemPaths: Send + Sync {
    fn config_file(&self) -> PathBuf;
    fn config_root(&self) -> PathBuf;
    fn mount_root(&self) -> PathBuf;
    fn service_root(&self) -> PathBuf;
    fn application_root(&self) -> PathBuf;
    fn log_path(&self, service_name: &str) -> PathBuf;
    fn log_path_std_out(&self, service_name: &str) -> PathBuf;
    fn log_path_std_err(&self, service_name: &str) -> PathBuf;
    fn log_root(&self) -> PathBuf;
    fn runtime_root(&self) -> PathBuf;
    fn pid_path(&self, service_name: &str) -> PathBuf;
    fn douglas_socket_path(&self, service_name: &str) -> PathBuf;
    fn token_path(&self) -> PathBuf;
    fn temp_root(&self) -> PathBuf;
    fn docker_socket_path(&self) -> PathBuf;
}

#[allow(dead_code)]
struct LinuxSystemPaths {}

impl LinuxSystemPaths {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {}
    }
}

impl SystemPaths for LinuxSystemPaths {
    fn config_file(&self) -> PathBuf {
        let mut result = self.config_root();
        result.push(PathBuf::from("config.json"));
        result
    }

    fn config_root(&self) -> PathBuf {
        PathBuf::from(format!("/etc/{APPLICATION_NAME}/"))
    }

    fn application_root(&self) -> PathBuf {
        PathBuf::from(format!("/var/lib/{APPLICATION_NAME}/"))
    }

    fn mount_root(&self) -> PathBuf {
        PathBuf::from(format!("/var/lib/{APPLICATION_NAME}/mounts/"))
    }

    fn service_root(&self) -> PathBuf {
        PathBuf::from(format!("/var/lib/{APPLICATION_NAME}/services/"))
    }

    fn log_root(&self) -> PathBuf {
        PathBuf::from(format!("/var/log/{APPLICATION_NAME}/"))
    }

    fn log_path(&self, service_name: &str) -> PathBuf {
        let mut result = self.log_root();
        result.push(PathBuf::from(format!("{service_name}.log")));
        result
    }

    fn log_path_std_out(&self, service_name: &str) -> PathBuf {
        let mut result = self.log_root();
        result.push(PathBuf::from(format!("{service_name}-stdout.log")));
        result
    }

    fn log_path_std_err(&self, service_name: &str) -> PathBuf {
        let mut result = self.log_root();
        result.push(PathBuf::from(format!("{service_name}-stderr.log")));
        result
    }

    fn runtime_root(&self) -> PathBuf {
        PathBuf::from(format!("/run/{APPLICATION_NAME}/"))
    }

    fn douglas_socket_path(&self, service_name: &str) -> PathBuf {
        let mut result = self.runtime_root();
        result.push(PathBuf::from(format!("{service_name}.sock")));
        result
    }

    fn pid_path(&self, service_name: &str) -> PathBuf {
        let mut result = self.runtime_root();
        result.push(PathBuf::from(format!("{service_name}.pid")));
        result
    }

    fn token_path(&self) -> PathBuf {
        let mut result = self.runtime_root();
        result.push(PathBuf::from("bract.token"));

        result
    }

    fn temp_root(&self) -> PathBuf {
        PathBuf::from(format!("/var/cache/{APPLICATION_NAME}/"))
    }

    fn docker_socket_path(&self) -> PathBuf {
        PathBuf::from("/run/docker.sock")
    }
}

struct MacOsSystemPaths {}

impl MacOsSystemPaths {
    pub fn new() -> Self {
        Self {}
    }
}

impl SystemPaths for MacOsSystemPaths {
    fn config_root(&self) -> PathBuf {
        PathBuf::from(format!("/Library/Preferences/{APPLICATION_NAME}/"))
    }

    fn config_file(&self) -> PathBuf {
        let mut result = self.config_root();
        result.push(PathBuf::from("config.plist"));
        result
    }

    fn mount_root(&self) -> PathBuf {
        PathBuf::from(format!(
            "/Library/Application Support/{APPLICATION_NAME}/mounts/"
        ))
    }

    fn service_root(&self) -> PathBuf {
        PathBuf::from(format!(
            "/Library/Application Support/{APPLICATION_NAME}/services/"
        ))
    }

    fn application_root(&self) -> PathBuf {
        PathBuf::from(format!("/Library/Application Support/{APPLICATION_NAME}/"))
    }

    fn log_root(&self) -> PathBuf {
        PathBuf::from(format!("/Library/Logs/{APPLICATION_NAME}/"))
    }

    fn log_path(&self, service_name: &str) -> PathBuf {
        let mut result = self.log_root();
        result.push(PathBuf::from(format!("{service_name}.log")));
        result
    }

    fn log_path_std_out(&self, service_name: &str) -> PathBuf {
        let mut result = self.log_root();
        result.push(PathBuf::from(format!("{service_name}-output.log")));
        result
    }

    fn log_path_std_err(&self, service_name: &str) -> PathBuf {
        let mut result = self.log_root();
        result.push(PathBuf::from(format!("{service_name}-error.log")));
        result
    }

    fn runtime_root(&self) -> PathBuf {
        PathBuf::from("/var/run/{APPLICATION_NAME}/")
    }

    fn douglas_socket_path(&self, service_name: &str) -> PathBuf {
        let mut result = self.runtime_root();
        result.push(PathBuf::from(format!("{service_name}.sock",)));
        result
    }

    fn pid_path(&self, service_name: &str) -> PathBuf {
        let mut result = self.runtime_root();
        result.push(PathBuf::from(format!("{service_name}.pid")));
        result
    }

    fn token_path(&self) -> PathBuf {
        let mut result = self.runtime_root();
        result.push(PathBuf::from("bract.token"));
        result
    }

    fn temp_root(&self) -> PathBuf {
        PathBuf::from(format!("/Library/Caches/{APPLICATION_NAME}/"))
    }

    fn docker_socket_path(&self) -> PathBuf {
        PathBuf::from("/var/run/docker.sock")
    }
}

#[cfg(target_os = "macos")]
pub fn create_system_paths() -> Box<dyn SystemPaths> {
    Box::new(MacOsSystemPaths::new())
}

#[cfg(target_os = "linux")]
pub fn create_system_paths() -> Box<dyn SystemPaths> {
    Box::new(LinuxSystemPaths::new())
}
