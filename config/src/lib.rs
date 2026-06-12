use std::path::PathBuf;
pub struct DouglasFolders {
    pub logs: PathBuf,
    pub transients: PathBuf,
    pub applications: PathBuf,
    pub application_services: PathBuf,
    pub application_mounts: PathBuf,
    pub configs: PathBuf,
    pub resin: PathBuf,
}

impl DouglasFolders {
    pub fn log_file(&self, name: &str) -> PathBuf {
        let mut result = self.logs.clone();
        result.push(format!("{name}.log"));
        result
    }

    pub fn socket_file(&self, name: &str) -> PathBuf {
        let mut result = self.transients.clone();
        result.push(format!("{name}.sock"));
        result
    }
}

impl Default for DouglasFolders {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
#[cfg(target_os = "macos")]
impl DouglasFolders {
    pub fn new() -> Self {
        Self {
            logs: PathBuf::from("/Library/Logs/douglas/"),
            transients: PathBuf::from("/var/run/douglas/"),
            applications: PathBuf::from("/Library/Application Support/douglas/"),
            application_services: PathBuf::from("/Library/Application Support/douglas/services/"),
            application_mounts: PathBuf::from("/Library/Application Support/douglas/mounts/"),
            configs: PathBuf::from("/Library/Preferences/douglas/"),
            resin: PathBuf::from("/Library/Application Support/douglas/resin/"),
        }
    }
}

#[allow(dead_code)]
#[cfg(target_os = "linux")]
impl DouglasFolders {
    pub fn new() -> Self {
        Self {
            logs: PathBuf::from("/var/log/douglas/"),
            transients: PathBuf::from("/run/douglas/"),
            applications: PathBuf::from("/var/lib/douglas/"),
            application_services: PathBuf::from("/var/lib/douglas/services/"),
            application_mounts: PathBuf::from("/var/lib/douglas/mounts/"),
            configs: PathBuf::from("/etc/douglas/"),
            resin: PathBuf::from("/var/lib/douglas/resin/"),
        }
    }
}
