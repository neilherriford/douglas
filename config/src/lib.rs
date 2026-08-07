use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct DouglasFolders {
    pub logs: PathBuf,
    pub transients: PathBuf,
    pub applications: PathBuf,
    pub application_services: PathBuf,
    pub application_mounts: PathBuf,
    pub configs: PathBuf,
    pub rolodex: PathBuf,
    services_root: PathBuf,
}

impl DouglasFolders {
    pub fn log_file(&self, name: &str) -> PathBuf {
        let mut result = self.logs.clone();
        result.push(format!("{name}.log"));
        result
    }

    pub fn log_dir(&self, name: &str) -> PathBuf {
        let mut result = self.logs.clone();
        result.push(name);
        result
    }

    pub fn service_log_file(&self, name: &str) -> PathBuf {
        let mut result = self.log_dir(name);
        result.push(format!("{name}.log"));
        result
    }

    pub fn socket_dir(&self, name: &str) -> PathBuf {
        let mut result = self.transients.clone();
        result.push(name);
        result
    }

    pub fn socket_file(&self, name: &str) -> PathBuf {
        let mut result = self.socket_dir(name);
        result.push(format!("{name}.sock"));
        result
    }

    pub fn service_root(&self, name: &str) -> PathBuf {
        let mut result = self.services_root.clone();
        result.push(name);
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
            rolodex: PathBuf::from("/Library/Application Support/douglas/rolodex/"),
            services_root: PathBuf::from("/Library/Application Support/douglas/"),
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
            rolodex: PathBuf::from("/var/lib/douglas/rolodex/"),
            services_root: PathBuf::from("/var/lib/douglas/"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DouglasFolders;
    use std::path::PathBuf;

    fn folders() -> DouglasFolders {
        DouglasFolders {
            logs: PathBuf::from("/var/log/douglas/"),
            transients: PathBuf::from("/run/douglas/"),
            applications: PathBuf::from("/var/lib/douglas/"),
            application_services: PathBuf::from("/var/lib/douglas/services/"),
            application_mounts: PathBuf::from("/var/lib/douglas/mounts/"),
            configs: PathBuf::from("/etc/douglas/"),
            rolodex: PathBuf::from("/var/lib/douglas/rolodex/"),
            services_root: PathBuf::from("/var/lib/douglas/"),
        }
    }

    #[test]
    fn test_log_file_should_be_flat_under_logs() {
        assert_eq!(
            folders().log_file("bract"),
            PathBuf::from("/var/log/douglas/bract.log")
        );
    }

    #[test]
    fn test_log_dir_should_be_nested_under_logs() {
        assert_eq!(
            folders().log_dir("bract"),
            PathBuf::from("/var/log/douglas/bract")
        );
    }

    #[test]
    fn test_service_log_file_should_be_nested_under_its_own_log_dir() {
        assert_eq!(
            folders().service_log_file("bract"),
            PathBuf::from("/var/log/douglas/bract/bract.log")
        );
    }

    #[test]
    fn test_socket_dir_should_be_nested_under_transients() {
        assert_eq!(
            folders().socket_dir("seedbank"),
            PathBuf::from("/run/douglas/seedbank")
        );
    }

    #[test]
    fn test_socket_file_should_be_nested_under_its_own_socket_dir() {
        assert_eq!(
            folders().socket_file("seedbank"),
            PathBuf::from("/run/douglas/seedbank/seedbank.sock")
        );
    }

    #[test]
    fn test_service_root_should_be_nested_under_applications() {
        assert_eq!(
            folders().service_root("resin"),
            PathBuf::from("/var/lib/douglas/resin")
        );
    }

    #[test]
    fn test_different_service_names_should_not_collide() {
        let folders = folders();

        assert_ne!(folders.log_dir("bract"), folders.log_dir("resin"));
        assert_ne!(folders.socket_dir("bract"), folders.socket_dir("resin"));
        assert_ne!(folders.service_root("bract"), folders.service_root("resin"));
    }
}
