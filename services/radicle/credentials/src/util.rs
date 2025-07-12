use crate::directory::Directory;
#[cfg(target_os = "linux")]
use crate::linux_directory::LinuxDirectory;
#[cfg(target_os = "macos")]
use crate::macos_directory::MacOSDirectory;
use crate::os::Os;
use std::sync::Arc;

#[cfg(target_os = "macos")]
pub fn create_for_target(os: Arc<dyn Os + 'static>) -> impl Directory + Sync + Send {
    MacOSDirectory::new(os.clone())
}

#[cfg(target_os = "linux")]
pub fn create_for_target(os: Arc<dyn Os + 'static>) -> impl Directory + Sync + Send {
    LinuxDirectory::new(os.clone())
}
