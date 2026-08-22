use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct DouglasFolders {
    pub logs: PathBuf,
    pub transients: PathBuf,
    pub configs: PathBuf,
    pub seedlings_root: PathBuf,
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

    pub fn seedling_root(&self, name: &str) -> PathBuf {
        let mut result = self.seedlings_root.clone();
        result.push(name);
        result
    }

    pub fn seedling_mounts(&self) -> PathBuf {
        let mut result = self.seedlings_root.clone();
        result.push("mounts");
        result
    }

    pub fn seedling_mount(&self, seedling_name: &str, mount_name: &str) -> PathBuf {
        let mut result = self.seedlings_root.clone();
        result.push("mounts");
        result.push(seedling_name);
        result.push(mount_name);
        result
    }

    pub fn services(&self) -> PathBuf {
        let mut result = self.seedlings_root.clone();
        result.push("services");
        result
    }

    pub fn rolodex(&self) -> PathBuf {
        let mut result = self.seedlings_root.clone();
        result.push("rolodex");
        result
    }

    pub fn credentials(&self) -> PathBuf {
        let mut result = self.seedlings_root.clone();
        result.push("credentials");
        result
    }

    pub fn credential_file(&self, name: &str) -> PathBuf {
        let mut result = self.credentials();
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
            configs: PathBuf::from("/Library/Preferences/douglas/"),
            seedlings_root: PathBuf::from("/Library/Application Support/douglas/"),
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
            configs: PathBuf::from("/etc/douglas/"),
            seedlings_root: PathBuf::from("/var/lib/douglas/"),
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
            configs: PathBuf::from("/etc/douglas/"),
            seedlings_root: PathBuf::from("/var/lib/douglas/"),
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
    fn test_seedling_root_should_be_nested_under_seedlings_root() {
        assert_eq!(
            folders().seedling_root("resin"),
            PathBuf::from("/var/lib/douglas/resin")
        );
    }

    #[test]
    fn test_seedling_mounts_should_be_nested_under_seedlings_root() {
        assert_eq!(
            folders().seedling_mounts(),
            PathBuf::from("/var/lib/douglas/mounts")
        );
    }

    #[test]
    fn test_seedling_mount_should_be_nested_under_the_seedlings_own_mount_dir() {
        assert_eq!(
            folders().seedling_mount("openbao", "socket"),
            PathBuf::from("/var/lib/douglas/mounts/openbao/socket")
        );
    }

    #[test]
    fn test_services_should_be_nested_under_seedlings_root() {
        assert_eq!(
            folders().services(),
            PathBuf::from("/var/lib/douglas/services")
        );
    }

    #[test]
    fn test_rolodex_should_be_nested_under_seedlings_root() {
        assert_eq!(
            folders().rolodex(),
            PathBuf::from("/var/lib/douglas/rolodex")
        );
    }

    #[test]
    fn test_credentials_should_be_nested_under_seedlings_root() {
        assert_eq!(
            folders().credentials(),
            PathBuf::from("/var/lib/douglas/credentials")
        );
    }

    #[test]
    fn test_credential_file_should_be_nested_under_credentials() {
        assert_eq!(
            folders().credential_file("openbao-approle"),
            PathBuf::from("/var/lib/douglas/credentials/openbao-approle")
        );
    }

    #[test]
    fn test_different_service_names_should_not_collide() {
        let folders = folders();

        assert_ne!(folders.log_dir("bract"), folders.log_dir("resin"));
        assert_ne!(folders.socket_dir("bract"), folders.socket_dir("resin"));
        assert_ne!(folders.seedling_root("bract"), folders.seedling_root("resin"));
        assert_ne!(
            folders.seedling_mount("bract", "socket"),
            folders.seedling_mount("resin", "socket")
        );
    }
}
